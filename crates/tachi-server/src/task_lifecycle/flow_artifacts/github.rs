use super::refs::initial_merge_state_for_pr;
use super::*;

pub(crate) fn write_intake_flow_artifacts(
    flow_id: &str,
    objective: &str,
    issue: &IssueSnapshot,
    automation_plan: &Value,
) -> Result<(), String> {
    let run_dir = run_dir_for_flow_id(flow_id)?;
    std::fs::create_dir_all(run_dir.join("artifacts"))
        .map_err(|e| format!("create intake run dir: {e}"))?;
    let now = Utc::now().to_rfc3339();
    let pr_handoff_path = run_dir.join("pr_handoff.md");
    let pr_handoff_path_string = pr_handoff_path.to_string_lossy().to_string();
    let pr_handoff = build_pr_handoff_body(
        flow_id,
        objective,
        Some(&format!("{}#{}", issue.repo, issue.number)),
        &json!({
            "task": objective,
            "issue_ref": format!("{}#{}", issue.repo, issue.number),
            "automation_plan": automation_plan,
        }),
        None,
        &string_array_field(automation_plan, "leader_gate_reasons"),
    );
    write_text_atomic(&pr_handoff_path, &pr_handoff)?;
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
            "automation_plan": automation_plan,
            "branch": automation_plan.get("branch").and_then(Value::as_str),
            "pr_title": automation_plan.get("pr_title").and_then(Value::as_str),
            "pr_handoff_path": pr_handoff_path_string.clone(),
            "artifacts": { "pr_handoff": pr_handoff_path_string },
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
            "labels": issue.labels,
            "automation_plan": automation_plan,
        }),
    )?;
    write_intake_instruction(&run_dir, flow_id, objective, issue, automation_plan)?;
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
            "automation_plan": automation_plan,
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

pub(in crate::task_lifecycle) fn intake_briefing_params(
    params: &TachiTaskParams,
    flow_id: &str,
    objective: &str,
    issue: &IssueSnapshot,
) -> TachiTaskParams {
    // kckylechen1/tachi#1058: the outer intake call's `format` (e.g. "full")
    // is the caller's only signal for wanting the full briefing board — it
    // gets overwritten to "json" below (the inner briefing call always wants
    // a machine-readable payload, not markdown). Read that intent BEFORE the
    // overwrite and, when `compact` itself was left unset, reverse-pressure
    // it into an explicit `compact = Some(false)` so the inner briefing call
    // doesn't fall back to its own `compact` default and silently clip a
    // `format="full"` intake down to the 4-row packet.
    let wants_full = crate::facade_memory_ops::wants_full_format(params.format.as_deref());
    let mut briefing = params.clone();
    briefing.action = crate::tool_params::TachiTaskAction::Briefing;
    briefing.format = Some("json".to_string());
    if briefing.compact.is_none() && wants_full {
        briefing.compact = Some(false);
    }
    briefing.flow_id = Some(flow_id.to_string());
    briefing.issue_ref = Some(format!("{}#{}", issue.repo, issue.number));
    briefing.task = Some(objective.to_string());
    briefing.doc_paths = issue.doc_paths.clone();
    briefing.spec_paths = issue.spec_paths.clone();
    briefing
}
