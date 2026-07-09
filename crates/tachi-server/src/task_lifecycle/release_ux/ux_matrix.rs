use super::status::{github_string, release_note_issue_ref, release_note_pr_ref};
use super::*;

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
        // F2 (#495/#913): GH PR lifecycle is canonical on tachi_gh.
        "tachi_gh(action='link_pr')",
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
        "tachi_gh(action='pr_status')",
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
        "tachi_gh(action='release_note')",
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
