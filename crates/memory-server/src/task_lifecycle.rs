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

pub(crate) async fn handle_task_release_note(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    let flow_id = params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let (run_dir, status) = match flow_id {
        Some(flow_id) => {
            let run_dir = run_dir_for_flow_id(flow_id)?;
            let status = read_json_file(&run_dir.join("status.json"))?
                .ok_or_else(|| format!("flow status not found for flow_id '{flow_id}'"))?;
            (Some(run_dir), status)
        }
        None => (None, json!({})),
    };
    let pr = if let Some(pr) = pr_snapshot_from_status(&status) {
        reject_release_note_pr_mismatch(params, &pr)?;
        Some(pr)
    } else if flow_id.is_none() || params.pr_ref.is_some() {
        Some(read_pr_snapshot(server, &resolve_task_release_note_pr_target(params)?).await?)
    } else {
        None
    };
    if flow_id.is_none() && pr.is_none() {
        return Err(
            "release_note requires flow_id or pr_ref='owner/repo#123' / GitHub PR URL".to_string(),
        );
    }
    let mut doc_paths = params.doc_paths.clone();
    let mut spec_paths = params.spec_paths.clone();
    if let Some(flow_id) = flow_id {
        let (flow_docs, flow_specs) = flow_status_doc_refs(Some(flow_id))?;
        doc_paths.extend(flow_docs);
        spec_paths.extend(flow_specs);
    }
    dedupe_strings(&mut doc_paths);
    dedupe_strings(&mut spec_paths);
    let verification = match flow_id {
        Some(flow_id) => crate::verify_ops::read_verification_ledger(flow_id)?,
        None => None,
    };
    let release_note = build_release_note_markdown(
        flow_id,
        &status,
        pr.as_ref(),
        &doc_paths,
        &spec_paths,
        verification.as_ref(),
    );
    let release_note_path = if let (Some(flow_id), Some(run_dir)) = (flow_id, run_dir.as_ref()) {
        let path = run_dir.join("release_note.md");
        write_text_atomic(&path, &release_note)?;
        let path_string = path.to_string_lossy().to_string();
        merge_flow_status(
            run_dir,
            json!({
                "flow_id": flow_id,
                "stage": "ship",
                "state": "release_note_generated",
                "release_note_path": path_string,
                "artifacts": { "release_note": path_string },
                "updated_at": Utc::now().to_rfc3339(),
            }),
        )?;
        Some(path.to_string_lossy().to_string())
    } else {
        None
    };
    serde_json::to_string(&json!({
        "ok": true,
        "action": "release_note",
        "flow_id": flow_id,
        "issue_ref": release_note_issue_ref(&status),
        "pr_ref": pr.as_ref().map(|pr| format!("{}#{}", pr.repo, pr.number))
            .or_else(|| release_note_pr_ref(&status)),
        "release_note_path": release_note_path,
        "release_note": release_note,
        "inputs": {
            "source": if flow_id.is_some() { "flow_status" } else { "github_pr" },
            "doc_paths": doc_paths,
            "spec_paths": spec_paths,
            "verification_present": verification.is_some(),
            "github_merge_state": github_string(&status, "merge_state"),
        },
    }))
    .map_err(|e| format!("serialize release_note: {e}"))
}

pub(crate) fn handle_task_ux_matrix(params: &TachiTaskParams) -> Result<String, String> {
    let flow_id = params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let run_dir = flow_id.map(run_dir_for_flow_id).transpose()?;
    if let Some(run_dir) = run_dir.as_ref() {
        std::fs::create_dir_all(run_dir)
            .map_err(|e| format!("create flow run dir for ux_matrix: {e}"))?;
    }
    let status = match run_dir.as_ref() {
        Some(run_dir) => read_json_file(&run_dir.join("status.json"))?.unwrap_or_else(|| json!({})),
        None => json!({}),
    };
    let mut doc_paths = params.doc_paths.clone();
    let mut spec_paths = params.spec_paths.clone();
    if let Some(flow_id) = flow_id {
        let (flow_docs, flow_specs) = flow_status_doc_refs(Some(flow_id))?;
        doc_paths.extend(flow_docs);
        spec_paths.extend(flow_specs);
    }
    dedupe_strings(&mut doc_paths);
    dedupe_strings(&mut spec_paths);

    let task = params
        .task
        .clone()
        .or_else(|| status_string(&status, "task"))
        .unwrap_or_else(|| "Tachi feature workflow".to_string());
    let issue_ref = params
        .issue_ref
        .clone()
        .or_else(|| release_note_issue_ref(&status));
    let pr_ref = params
        .pr_ref
        .clone()
        .or_else(|| release_note_pr_ref(&status));
    let verification = match flow_id {
        Some(flow_id) => crate::verify_ops::read_verification_ledger(flow_id)?,
        None => None,
    };

    let instruction_exists = run_dir
        .as_ref()
        .is_some_and(|dir| dir.join("instruction.md").exists());
    let status_exists = run_dir
        .as_ref()
        .is_some_and(|dir| dir.join("status.json").exists());
    let dispatch_ids = string_array_field(&status, "dispatch_ids");
    let completed_dispatch_ids = string_array_field(&status, "completed_dispatch_ids");
    let merge_state = github_string(&status, "merge_state");
    let pr_status_seen = status.get("github").is_some_and(|github| {
        github.get("requested_mode").is_some()
            || github.get("policy").is_some()
            || github.get("will_merge").is_some()
            || github.get("head_consistency").is_some()
    });
    let release_note_path = status_string(&status, "release_note_path").or_else(|| {
        status
            .get("artifacts")
            .and_then(|artifacts| artifacts.get("release_note"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let release_note_exists = release_note_path
        .as_deref()
        .is_some_and(|path| Path::new(path).exists());
    let verification_overall = verification
        .as_ref()
        .and_then(|ledger| ledger.get("overall"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let close_loop_done = status_string(&status, "state")
        .as_deref()
        .is_some_and(|state| UX_CLOSURE_STATES.contains(&state));

    let mut matrix = Vec::new();
    matrix.push(ux_step(
        "intake",
        "Issue intake",
        "tachi_task(action='intake')",
        if status_exists && issue_ref.is_some() && instruction_exists {
            "passed"
        } else if issue_ref.is_some() {
            "ready"
        } else {
            "pending"
        },
        vec_if([
            (status_exists, "status.json exists"),
            (instruction_exists, "instruction.md exists"),
            (issue_ref.is_some(), "issue_ref linked"),
        ]),
        gaps_if([
            (!status_exists, "no flow status.json"),
            (!instruction_exists, "no intake instruction.md"),
            (issue_ref.is_none(), "no issue_ref"),
        ]),
        "Bind a GitHub issue and create flow artifacts.",
        true,
    ));
    matrix.push(ux_step(
        "canonical_docs",
        "Canonical docs/specs",
        "tachi_task(action='briefing')",
        if !doc_paths.is_empty() || !spec_paths.is_empty() {
            "passed"
        } else {
            "pending"
        },
        doc_paths
            .iter()
            .map(|path| format!("doc:{path}"))
            .chain(spec_paths.iter().map(|path| format!("spec:{path}")))
            .collect(),
        if doc_paths.is_empty() && spec_paths.is_empty() {
            vec!["no canonical docs/specs attached".to_string()]
        } else {
            Vec::new()
        },
        "Attach or create canonical docs/specs before treating memory as feature truth.",
        true,
    ));
    matrix.push(ux_step(
        "briefing",
        "Feature briefing",
        "tachi_task(action='briefing')",
        if flow_id.is_some() || issue_ref.is_some() {
            "ready"
        } else {
            "pending"
        },
        vec!["read-only action; rerun to inspect current feature board".to_string()],
        gaps_if([(
            flow_id.is_none() && issue_ref.is_none(),
            "needs flow_id or issue_ref",
        )]),
        "Read the feature board before dispatching work.",
        true,
    ));
    matrix.push(ux_step(
        "recommend",
        "Route recommendation",
        "tachi_task(action='recommend')",
        if !task.trim().is_empty() {
            "ready"
        } else {
            "pending"
        },
        vec!["read-only action; consumes task, risk, docs/spec paths, and live eval".to_string()],
        gaps_if([(task.trim().is_empty(), "task text is missing")]),
        "Choose a profile before assigning external workers.",
        true,
    ));
    matrix.push(ux_step(
        "dispatch",
        "Worker dispatch",
        "tachi_task(action='dispatch')",
        if !dispatch_ids.is_empty() {
            "passed"
        } else if flow_id.is_some() {
            "ready"
        } else {
            "pending"
        },
        dispatch_ids
            .iter()
            .map(|id| format!("dispatch_id:{id}"))
            .collect(),
        gaps_if([(
            dispatch_ids.is_empty(),
            "no dispatch_ids recorded in flow status",
        )]),
        "Dispatch a bounded worker slice with profile, flow_id, issue_ref, and evidence requirements.",
        true,
    ));
    matrix.push(ux_step(
        "board",
        "Worker board",
        "tachi_task(action='board')",
        if !dispatch_ids.is_empty() {
            "ready"
        } else {
            "pending"
        },
        vec!["read-only action; poll worker state after dispatch".to_string()],
        gaps_if([(dispatch_ids.is_empty(), "no workers to poll yet")]),
        "Poll dispatched workers until result/evidence is available.",
        false,
    ));
    matrix.push(ux_step(
        "complete_eval",
        "Completion / eval linkage",
        "tachi_task(action='complete', dispatch_id=..., flow_id=...)",
        if !completed_dispatch_ids.is_empty() {
            "passed"
        } else if !dispatch_ids.is_empty() {
            "ready"
        } else {
            "pending"
        },
        completed_dispatch_ids
            .iter()
            .map(|id| format!("completed_dispatch_id:{id}"))
            .collect(),
        gaps_if([
            (dispatch_ids.is_empty(), "no dispatched workers yet"),
            (
                !dispatch_ids.is_empty() && completed_dispatch_ids.is_empty(),
                "no tachi_complete eval linked to a dispatch card",
            ),
        ]),
        "Call tachi_task(action='complete') with flow_id and dispatch_id so /eval evidence links back to the dispatch card.",
        true,
    ));
    matrix.push(ux_step(
        "verification",
        "Verification ledger",
        "tachi_verify(action='board')",
        match verification_overall.as_deref() {
            Some("passed") => "passed",
            Some("failed") => "blocked",
            Some("pending" | "running") => "pending",
            Some(_) => "pending",
            None if flow_id.is_some() => "ready",
            None => "pending",
        },
        verification_overall
            .as_ref()
            .map(|overall| vec![format!("overall:{overall}")])
            .unwrap_or_default(),
        gaps_if([(verification.is_none(), "no verification.json ledger")]),
        "Record gitleaks, cargo check, clippy, tests, and other required gates.",
        true,
    ));
    matrix.push(ux_step(
        "link_pr",
        "PR linkage",
        "tachi_task(action='link_pr')",
        if pr_ref.is_some() {
            "passed"
        } else if flow_id.is_some() {
            "ready"
        } else {
            "pending"
        },
        pr_ref
            .as_ref()
            .map(|pr_ref| vec![format!("pr_ref:{pr_ref}")])
            .unwrap_or_default(),
        gaps_if([(pr_ref.is_none(), "no pr_ref linked")]),
        "Attach the GitHub PR to the flow before safe-merge checks.",
        true,
    ));
    matrix.push(ux_step(
        "pr_status",
        "PR safe-merge preview",
        "tachi_task(action='pr_status')",
        if matches!(merge_state.as_deref(), Some("blocked")) {
            "blocked"
        } else if pr_status_seen {
            "passed"
        } else if pr_ref.is_some() {
            "ready"
        } else {
            "pending"
        },
        merge_state
            .as_ref()
            .map(|state| vec![format!("merge_state:{state}")])
            .unwrap_or_default(),
        gaps_if([(!pr_status_seen, "no persisted safe-merge preview")]),
        "Preview GitHub checks/review/verification before merge.",
        true,
    ));
    matrix.push(ux_step(
        "release_note",
        "Release note",
        "tachi_task(action='release_note')",
        if release_note_exists {
            "passed"
        } else if flow_id.is_some() {
            "ready"
        } else {
            "pending"
        },
        release_note_path
            .as_ref()
            .map(|path| vec![format!("release_note:{path}")])
            .unwrap_or_default(),
        gaps_if([(!release_note_exists, "no release_note.md artifact")]),
        "Generate a release note from flow, GitHub, docs, and verification evidence.",
        true,
    ));
    matrix.push(ux_step(
        "close_loop",
        "Closure synthesis",
        "tachi_task(action='close_loop')",
        if close_loop_done {
            "passed"
        } else if release_note_exists && !matches!(verification_overall.as_deref(), Some("failed"))
        {
            "ready"
        } else {
            "pending"
        },
        vec_if([(close_loop_done, "flow state indicates closure")]),
        gaps_if([(!close_loop_done, "no persisted close_loop marker")]),
        "Promote durable docs/wiki/memory lessons after review.",
        true,
    ));

    let blocked = matrix
        .iter()
        .any(|step| step.get("status").and_then(Value::as_str) == Some("blocked"));
    let required_pending = matrix.iter().any(|step| {
        step.get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && matches!(step.get("status").and_then(Value::as_str), Some("pending"))
    });
    let overall = if blocked {
        "blocked"
    } else if close_loop_done {
        "complete"
    } else if release_note_exists && pr_status_seen {
        "ready_for_close_loop"
    } else if required_pending {
        "needs_action"
    } else {
        "ready"
    };
    let next_action = matrix
        .iter()
        .find(|step| {
            step.get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && matches!(
                    step.get("status").and_then(Value::as_str),
                    Some("pending" | "ready" | "blocked")
                )
        })
        .and_then(|step| step.get("next_action"))
        .and_then(Value::as_str)
        .unwrap_or("Continue with the next lifecycle action.")
        .to_string();
    let ux_matrix_path = run_dir
        .as_ref()
        .map(|dir| dir.join("ux_matrix.json").to_string_lossy().to_string());
    let result = json!({
        "ok": true,
        "action": "ux_matrix",
        "kind": "feature_workflow_ux_matrix",
        "flow_id": flow_id,
        "issue_ref": issue_ref,
        "pr_ref": pr_ref,
        "task": task,
        "overall": overall,
        "next_action": next_action,
        "ux_matrix_path": ux_matrix_path,
        "matrix": matrix,
        "inputs": {
            "doc_paths": doc_paths,
            "spec_paths": spec_paths,
            "verification_present": verification.is_some(),
            "pr_status_seen": pr_status_seen,
            "github_merge_state": merge_state,
        }
    });
    if let (Some(run_dir), Some(path)) = (run_dir.as_ref(), ux_matrix_path.as_deref()) {
        let path = Path::new(path);
        if read_json_file(path)?.as_ref() != Some(&result) {
            write_json_atomic(path, &result)?;
            let path_string = path.to_string_lossy().to_string();
            merge_flow_status(
                run_dir,
                json!({
                    "flow_id": flow_id,
                    "artifacts": { "ux_matrix": path_string },
                    "ux_matrix_path": path_string,
                    "updated_at": Utc::now().to_rfc3339(),
                }),
            )?;
        }
    }
    serde_json::to_string(&result).map_err(|e| format!("serialize ux_matrix: {e}"))
}

pub(crate) fn mark_task_close_loop(flow_id: &str, raw_result: &str) -> Result<(), String> {
    let run_dir = run_dir_for_flow_id(flow_id)?;
    let close_loop_path = run_dir.join("close_loop.json");
    let payload = serde_json::from_str::<Value>(raw_result).unwrap_or_else(|_| {
        json!({
            "action": "close_loop",
            "raw_result": raw_result,
        })
    });
    write_json_atomic(&close_loop_path, &payload)?;
    let path_string = close_loop_path.to_string_lossy().to_string();
    merge_flow_status(
        &run_dir,
        json!({
            "state": "closed_loop",
            "closed_at": Utc::now().to_rfc3339(),
            "close_loop_path": path_string,
            "artifacts": { "close_loop": path_string },
        }),
    )?;
    Ok(())
}

pub(crate) fn mark_task_dispatch(
    flow_id: &str,
    dispatch_id: &str,
    mut card: Value,
) -> Result<(), String> {
    if !is_safe_dispatch_marker_id(dispatch_id) {
        return Err(format!(
            "invalid dispatch_id for flow marker: {dispatch_id}"
        ));
    }
    let _guard = FLOW_MARKER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let run_dir = run_dir_for_flow_id(flow_id)?;
    std::fs::create_dir_all(run_dir.join("artifacts"))
        .map_err(|e| format!("create dispatch artifact dir: {e}"))?;

    let recorded_at = Utc::now().to_rfc3339();
    if !card.is_object() {
        card = json!({ "details": card });
    }
    if let Some(obj) = card.as_object_mut() {
        obj.insert("flow_id".to_string(), json!(flow_id));
        obj.insert("dispatch_id".to_string(), json!(dispatch_id));
        obj.insert("recorded_at".to_string(), json!(recorded_at));
    }

    let card_path = run_dir
        .join("artifacts")
        .join(format!("dispatch-{dispatch_id}.json"));
    write_json_atomic(&card_path, &card)?;
    let card_path_string = card_path.to_string_lossy().to_string();

    let status_path = run_dir.join("status.json");
    let mut status = read_json_file(&status_path)?.unwrap_or_else(|| json!({}));
    if !status.is_object() {
        status = json!({});
    }
    let obj = status.as_object_mut().expect("status object");
    let dispatch_ids = obj
        .entry("dispatch_ids".to_string())
        .or_insert_with(|| json!([]));
    if !dispatch_ids.is_array() {
        *dispatch_ids = json!([]);
    }
    if let Some(ids) = dispatch_ids.as_array_mut() {
        let already_present = ids.iter().any(|value| value.as_str() == Some(dispatch_id));
        if !already_present {
            ids.push(json!(dispatch_id));
        }
    }
    let dispatch_cards = obj
        .entry("dispatch_cards".to_string())
        .or_insert_with(|| json!([]));
    if !dispatch_cards.is_array() {
        *dispatch_cards = json!([]);
    }
    if let Some(cards) = dispatch_cards.as_array_mut() {
        let already_present = cards
            .iter()
            .any(|value| value.as_str() == Some(card_path_string.as_str()));
        if !already_present {
            cards.push(json!(card_path_string));
        }
    }
    let artifacts = obj
        .entry("artifacts".to_string())
        .or_insert_with(|| json!({}));
    if !artifacts.is_object() {
        *artifacts = json!({});
    }
    if let Some(artifact_obj) = artifacts.as_object_mut() {
        let dispatch_artifacts = artifact_obj
            .entry("dispatches".to_string())
            .or_insert_with(|| json!({}));
        if !dispatch_artifacts.is_object() {
            *dispatch_artifacts = json!({});
        }
        if let Some(dispatch_obj) = dispatch_artifacts.as_object_mut() {
            dispatch_obj.insert(dispatch_id.to_string(), json!(card_path_string));
        }
    }
    obj.insert("stage".to_string(), json!("dispatch"));
    obj.insert("state".to_string(), json!("dispatched"));
    obj.insert("last_dispatch_id".to_string(), json!(dispatch_id));
    obj.insert("updated_at".to_string(), json!(recorded_at.clone()));
    if obj.get("created_at").is_none() {
        obj.insert("created_at".to_string(), json!(recorded_at.clone()));
    }
    write_json_atomic(&status_path, &status)?;

    append_flow_event(
        &run_dir,
        json!({
            "event": "dispatch_linked",
            "flow_id": flow_id,
            "dispatch_id": dispatch_id,
            "dispatch_card": card_path_string,
            "timestamp": recorded_at,
        }),
    )?;
    Ok(())
}

pub(crate) fn mark_task_dispatch_completion(
    flow_id: &str,
    dispatch_id: &str,
    mut completion: Value,
) -> Result<Value, String> {
    if !is_safe_dispatch_marker_id(dispatch_id) {
        return Err(format!(
            "invalid dispatch_id for flow completion marker: {dispatch_id}"
        ));
    }
    let _guard = FLOW_MARKER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let run_dir = run_dir_for_flow_id(flow_id)?;
    std::fs::create_dir_all(run_dir.join("artifacts"))
        .map_err(|e| format!("create dispatch artifact dir: {e}"))?;

    let completed_at = Utc::now().to_rfc3339();
    if !completion.is_object() {
        completion = json!({ "details": completion });
    }
    if let Some(obj) = completion.as_object_mut() {
        obj.insert("flow_id".to_string(), json!(flow_id));
        obj.insert("dispatch_id".to_string(), json!(dispatch_id));
        obj.insert("completed_at".to_string(), json!(completed_at.clone()));
    }

    let status_path = run_dir.join("status.json");
    let mut status = read_json_file(&status_path)?.unwrap_or_else(|| json!({}));
    if !status.is_object() {
        status = json!({});
    }

    let card_path = status
        .get("artifacts")
        .and_then(|artifacts| artifacts.get("dispatches"))
        .and_then(|dispatches| dispatches.get(dispatch_id))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            run_dir
                .join("artifacts")
                .join(format!("dispatch-{dispatch_id}.json"))
        });
    let mut card = read_json_file(&card_path)?.unwrap_or_else(|| {
        json!({
            "flow_id": flow_id,
            "dispatch_id": dispatch_id,
        })
    });
    if !card.is_object() {
        card = json!({ "details": card });
    }
    if let Some(obj) = card.as_object_mut() {
        obj.insert("flow_id".to_string(), json!(flow_id));
        obj.insert("dispatch_id".to_string(), json!(dispatch_id));
        obj.insert("completion".to_string(), completion.clone());
        let history = obj
            .entry("completion_history".to_string())
            .or_insert_with(|| json!([]));
        if !history.is_array() {
            *history = json!([]);
        }
        if let Some(items) = history.as_array_mut() {
            let incoming_eval_id = completion
                .get("eval_memory_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let incoming_task_id = completion
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let already_present = items.iter().any(|item| {
                let item_eval_id = item.get("eval_memory_id").and_then(Value::as_str);
                let item_task_id = item.get("task_id").and_then(Value::as_str);
                incoming_eval_id
                    .as_deref()
                    .is_some_and(|id| item_eval_id == Some(id))
                    || incoming_task_id
                        .as_deref()
                        .is_some_and(|id| item_task_id == Some(id))
            });
            if !already_present {
                items.push(completion.clone());
            }
        }
    }
    write_json_atomic(&card_path, &card)?;
    let card_path_string = card_path.to_string_lossy().to_string();

    let obj = status.as_object_mut().expect("status object");
    let dispatch_ids = obj
        .entry("dispatch_ids".to_string())
        .or_insert_with(|| json!([]));
    if !dispatch_ids.is_array() {
        *dispatch_ids = json!([]);
    }
    if let Some(ids) = dispatch_ids.as_array_mut() {
        let already_present = ids.iter().any(|value| value.as_str() == Some(dispatch_id));
        if !already_present {
            ids.push(json!(dispatch_id));
        }
    }
    let completed_ids = obj
        .entry("completed_dispatch_ids".to_string())
        .or_insert_with(|| json!([]));
    if !completed_ids.is_array() {
        *completed_ids = json!([]);
    }
    if let Some(ids) = completed_ids.as_array_mut() {
        let already_present = ids.iter().any(|value| value.as_str() == Some(dispatch_id));
        if !already_present {
            ids.push(json!(dispatch_id));
        }
    }
    let artifacts = obj
        .entry("artifacts".to_string())
        .or_insert_with(|| json!({}));
    if !artifacts.is_object() {
        *artifacts = json!({});
    }
    if let Some(artifact_obj) = artifacts.as_object_mut() {
        let dispatch_artifacts = artifact_obj
            .entry("dispatches".to_string())
            .or_insert_with(|| json!({}));
        if !dispatch_artifacts.is_object() {
            *dispatch_artifacts = json!({});
        }
        if let Some(dispatch_obj) = dispatch_artifacts.as_object_mut() {
            dispatch_obj.insert(dispatch_id.to_string(), json!(card_path_string.clone()));
        }
        let completion_artifacts = artifact_obj
            .entry("dispatch_completions".to_string())
            .or_insert_with(|| json!({}));
        if !completion_artifacts.is_object() {
            *completion_artifacts = json!({});
        }
        if let Some(completion_obj) = completion_artifacts.as_object_mut() {
            completion_obj.insert(dispatch_id.to_string(), completion.clone());
        }
    }
    let dispatch_eval = obj
        .entry("dispatch_eval".to_string())
        .or_insert_with(|| json!({}));
    if !dispatch_eval.is_object() {
        *dispatch_eval = json!({});
    }
    if let Some(eval_obj) = dispatch_eval.as_object_mut() {
        eval_obj.insert(dispatch_id.to_string(), completion.clone());
    }
    let current_stage = obj.get("stage").and_then(Value::as_str);
    if current_stage.is_none() || current_stage == Some("dispatch") {
        obj.insert("stage".to_string(), json!("eval"));
    }
    let current_state = obj.get("state").and_then(Value::as_str);
    if current_state.is_none() || current_state == Some("dispatched") {
        obj.insert("state".to_string(), json!("dispatch_completed"));
    }
    obj.insert("last_completed_dispatch_id".to_string(), json!(dispatch_id));
    obj.insert(
        "last_dispatch_completion_at".to_string(),
        json!(completed_at.clone()),
    );
    obj.insert("updated_at".to_string(), json!(completed_at.clone()));
    write_json_atomic(&status_path, &status)?;

    append_flow_event(
        &run_dir,
        json!({
            "event": "dispatch_completed",
            "flow_id": flow_id,
            "dispatch_id": dispatch_id,
            "dispatch_card": card_path_string,
            "completion": completion,
            "timestamp": completed_at,
        }),
    )?;

    Ok(json!({
        "recorded": true,
        "flow_id": flow_id,
        "dispatch_id": dispatch_id,
        "dispatch_card": card_path.to_string_lossy().to_string(),
    }))
}

fn is_safe_dispatch_marker_id(dispatch_id: &str) -> bool {
    !dispatch_id.trim().is_empty()
        && dispatch_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
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

fn resolve_task_release_note_pr_target(params: &TachiTaskParams) -> Result<GithubTarget, String> {
    resolve_task_pr_target(params).map_err(|_| {
        "release_note requires flow_id or pr_ref='owner/repo#123' / GitHub PR URL".to_string()
    })
}

fn reject_release_note_pr_mismatch(
    params: &TachiTaskParams,
    cached_pr: &PrSnapshot,
) -> Result<(), String> {
    let Some(pr_ref) = params
        .pr_ref
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
    };
    let target = parse_pr_ref(pr_ref).ok_or_else(|| {
        "release_note requires flow_id or pr_ref='owner/repo#123' / GitHub PR URL".to_string()
    })?;
    if target.repo != cached_pr.repo || target.number != cached_pr.number {
        return Err(format!(
            "release_note pr_ref mismatch: flow has '{}#{}', request supplied '{}#{}'",
            cached_pr.repo, cached_pr.number, target.repo, target.number
        ));
    }
    Ok(())
}

fn build_release_note_markdown(
    flow_id: Option<&str>,
    status: &Value,
    pr: Option<&PrSnapshot>,
    doc_paths: &[String],
    spec_paths: &[String],
    verification: Option<&Value>,
) -> String {
    let task = status
        .get("task")
        .and_then(Value::as_str)
        .or_else(|| pr.map(|pr| pr.title.as_str()))
        .unwrap_or("Tachi task lifecycle update");
    let mut body = String::new();
    body.push_str("# Release Note\n\n");
    body.push_str("## Summary\n\n");
    body.push_str(&format!("- {task}\n"));
    if let Some(flow_id) = flow_id {
        body.push_str(&format!("- Flow: `{flow_id}`\n"));
    }

    body.push_str("\n## GitHub\n\n");
    let issue_ref = release_note_issue_ref(status);
    let pr_ref = pr
        .map(|pr| format!("{}#{}", pr.repo, pr.number))
        .or_else(|| release_note_pr_ref(status));
    if let Some(issue_ref) = issue_ref.as_deref() {
        body.push_str(&format!("- Issue: `{issue_ref}`\n"));
    } else {
        body.push_str("- Issue: not linked\n");
    }
    if let Some(pr_ref) = pr_ref.as_deref() {
        body.push_str(&format!("- PR: `{pr_ref}`\n"));
    } else {
        body.push_str("- PR: not linked\n");
    }
    let pr_url = pr
        .map(|pr| pr.url.clone())
        .or_else(|| github_string(status, "pr_url"));
    if let Some(url) = pr_url.as_deref() {
        body.push_str(&format!("- PR URL: {url}\n"));
    }
    let pr_state = pr
        .and_then(|pr| pr.state.clone())
        .or_else(|| github_string(status, "pr_state"));
    if let Some(state) = pr_state.as_deref() {
        body.push_str(&format!("- PR state: `{state}`\n"));
    }
    if let Some(merge_state) = github_string(status, "merge_state") {
        body.push_str(&format!("- Merge state: `{merge_state}`\n"));
    }
    let review = pr
        .and_then(|pr| pr.review_decision.clone())
        .or_else(|| review_state(status));
    if let Some(review) = review.as_deref() {
        body.push_str(&format!("- Review: `{review}`\n"));
    }
    let mergeable = pr
        .and_then(|pr| pr.mergeable.clone())
        .or_else(|| github_string(status, "mergeable"));
    if let Some(mergeable) = mergeable.as_deref() {
        body.push_str(&format!("- Mergeable: `{mergeable}`\n"));
    }

    body.push_str("\n## Canonical Docs / Specs\n\n");
    if spec_paths.is_empty() && doc_paths.is_empty() {
        body.push_str("- No canonical docs/specs attached. Attach or create docs before treating memory as feature truth.\n");
    } else {
        for path in spec_paths {
            body.push_str(&format!("- spec: `{path}`\n"));
        }
        for path in doc_paths {
            body.push_str(&format!("- doc: `{path}`\n"));
        }
    }

    body.push_str("\n## Changes\n\n");
    if let Some(pr) = pr {
        body.push_str(&format!("- {}\n", pr.title));
        if let Some(head) = pr.head_ref.as_deref() {
            body.push_str(&format!("- Head branch: `{head}`\n"));
        }
        if let Some(base) = pr.base_ref.as_deref() {
            body.push_str(&format!("- Base branch: `{base}`\n"));
        }
    } else if let Some(pr_title) = github_string(status, "pr_title") {
        body.push_str(&format!("- {pr_title}\n"));
    } else {
        body.push_str("- Release note generated from flow state; no PR title was available.\n");
    }

    body.push_str("\n## Verification\n\n");
    append_verification_summary(&mut body, verification);

    body.push_str("\n## Follow-Up\n\n");
    if spec_paths.is_empty() && doc_paths.is_empty() {
        body.push_str("- Attach or create canonical docs/specs for this flow.\n");
    }
    if verification.is_none() {
        body.push_str("- Attach a verification ledger before safe merge or closure.\n");
    }
    if spec_paths.is_empty() && doc_paths.is_empty() && verification.is_none() {
        body.push_str("- Keep this note as a draft until docs and verification are attached.\n");
    } else {
        body.push_str("- Use `tachi_task(action='close_loop', ...)` to promote durable lessons after review.\n");
    }
    body
}

fn append_verification_summary(body: &mut String, verification: Option<&Value>) {
    let Some(verification) = verification else {
        body.push_str("- No verification ledger attached.\n");
        return;
    };
    if let Some(overall) = verification.get("overall").and_then(Value::as_str) {
        body.push_str(&format!("- Overall: `{overall}`\n"));
    }
    let items = verification
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if items.is_empty() {
        body.push_str("- Verification ledger exists but has no items.\n");
        return;
    }
    for item in items.iter().take(8) {
        let name = item
            .get("command")
            .and_then(Value::as_str)
            .or_else(|| item.get("kind").and_then(Value::as_str))
            .or_else(|| item.get("check_id").and_then(Value::as_str))
            .unwrap_or("verification");
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        body.push_str(&format!("- `{status}` {name}\n"));
    }
    if items.len() > 8 {
        body.push_str(&format!(
            "- ... {} more verification item(s)\n",
            items.len() - 8
        ));
    }
}

fn pr_snapshot_from_status(status: &Value) -> Option<PrSnapshot> {
    let github = status.get("github")?;
    let repo = github.get("repo").and_then(Value::as_str)?.to_string();
    let number = github.get("pr_number").and_then(Value::as_u64)?;
    Some(PrSnapshot {
        repo: repo.clone(),
        number,
        title: github
            .get("pr_title")
            .and_then(Value::as_str)
            .unwrap_or("GitHub PR")
            .to_string(),
        state: github
            .get("pr_state")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string),
        url: github
            .get("pr_url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("https://github.com/{repo}/pull/{number}")),
        head_ref: github
            .get("head_ref")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string),
        base_ref: github
            .get("base_ref")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string),
        review_decision: review_state(status),
        mergeable: github
            .get("mergeable")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string),
    })
}

fn release_note_issue_ref(status: &Value) -> Option<String> {
    status
        .get("issue_ref")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| github_string(status, "issue_ref"))
        .or_else(|| {
            let github = status.get("github")?;
            let repo = github.get("repo").and_then(Value::as_str)?;
            let number = github.get("issue_number").and_then(Value::as_u64)?;
            Some(format!("{repo}#{number}"))
        })
}

fn release_note_pr_ref(status: &Value) -> Option<String> {
    status
        .get("pr_ref")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| github_string(status, "pr_ref"))
        .or_else(|| {
            let github = status.get("github")?;
            let repo = github.get("repo").and_then(Value::as_str)?;
            let number = github.get("pr_number").and_then(Value::as_u64)?;
            Some(format!("{repo}#{number}"))
        })
}

fn github_string(status: &Value, key: &str) -> Option<String> {
    status
        .get("github")
        .and_then(|github| github.get(key))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn review_state(status: &Value) -> Option<String> {
    status
        .get("github")
        .and_then(|github| github.get("review"))
        .and_then(|review| review.get("state"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
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

fn ux_step(
    id: &str,
    label: &str,
    tool: &str,
    status: &str,
    evidence: Vec<String>,
    gaps: Vec<String>,
    next_action: &str,
    required: bool,
) -> Value {
    json!({
        "id": id,
        "label": label,
        "tool": tool,
        "status": status,
        "required": required,
        "evidence": evidence,
        "gaps": gaps,
        "next_action": next_action,
    })
}

fn vec_if<const N: usize>(items: [(bool, &'static str); N]) -> Vec<String> {
    items
        .into_iter()
        .filter_map(|(include, message)| include.then_some(message.to_string()))
        .collect()
}

fn gaps_if<const N: usize>(items: [(bool, &'static str); N]) -> Vec<String> {
    // Semantic alias for UX matrix call sites: evidence and gaps share shape.
    vec_if(items)
}

fn status_string(status: &Value, key: &str) -> Option<String> {
    status
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
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

pub(crate) fn read_json_file(path: &Path) -> Result<Option<Value>, String> {
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

fn write_text_atomic(path: &Path, body: &str) -> Result<(), String> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))
}

fn append_flow_event(run_dir: &Path, event: Value) -> Result<(), String> {
    use std::io::Write;
    let mut line =
        serde_json::to_string(&event).map_err(|e| format!("serialize flow event: {e}"))?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("events.jsonl"))
        .map_err(|e| format!("open events.jsonl: {e}"))?;
    file.write_all(line.as_bytes())
        .map_err(|e| format!("write events.jsonl: {e}"))
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
