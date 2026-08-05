use super::*;

pub(super) async fn read_issue_snapshot(
    server: &MemoryServer,
    target: &GithubTarget,
    params: &TachiTaskParams,
) -> Result<IssueSnapshot, String> {
    let raw = crate::gh_ops::handle_tachi_gh(
        server,
        TachiGhParams {
            action: "issue_read".to_string(),
            repo: Some(target.repo.clone()),
            number: Some(target.number),
            dry_run: Some(true),
            ..Default::default()
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
    let labels = result
        .get("labels")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("name")
                        .and_then(Value::as_str)
                        .or_else(|| item.as_str())
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
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
        body,
        labels,
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

pub(super) async fn read_pr_snapshot(
    server: &MemoryServer,
    target: &GithubTarget,
) -> Result<PrSnapshot, String> {
    let raw = crate::gh_ops::handle_tachi_gh(
        server,
        TachiGhParams {
            action: "pr_read".to_string(),
            repo: Some(target.repo.clone()),
            number: Some(target.number),
            dry_run: Some(true),
            ..Default::default()
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

pub(super) async fn seed_intake_orchestrator(
    server: &MemoryServer,
    flow_id: &str,
    objective: &str,
    issue: &IssueSnapshot,
    automation_plan: &Value,
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
    let dispatch_allowed = automation_plan
        .get("dispatch_allowed")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let leader_gate_reasons = string_array_field(automation_plan, "leader_gate_reasons");
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
                "Run cycle_status, then follow the next lifecycle step for briefing, harness-native execution, or PR handoff."
                    .to_string(),
            ),
            todo_status: Some(
                if dispatch_allowed {
                    "pending"
                } else {
                    "blocked"
                }
                .to_string(),
            ),
            parent_todo_id: None,
            agent: Some("tachi_task_intake".to_string()),
            issue_ref: Some(issue_ref.clone()),
            blocked_reason: if dispatch_allowed {
                None
            } else {
                Some(leader_gate_reasons.join(", "))
            },
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
                "Run cycle_status to select the next lifecycle step.".to_string(),
                if dispatch_allowed {
                    "Dispatch bounded worker slice.".to_string()
                } else {
                    "Get leader confirmation before dispatch.".to_string()
                },
                "Link PR and run pr_status gate before merge.".to_string(),
            ],
            files_touched: Vec::new(),
            commands_run: Vec::new(),
            tests_run: Vec::new(),
            known_blockers: leader_gate_reasons,
            next_action: Some(
                automation_plan
                    .get("recommended_next_action")
                    .and_then(Value::as_str)
                    .unwrap_or("Call tachi_task(action='cycle_status', flow_id=...) and follow the next lifecycle step.")
                    .to_string(),
            ),
            newest_user_instruction: None,
        },
    )
    .await?;
    Ok(())
}
