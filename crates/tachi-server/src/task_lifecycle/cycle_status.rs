use super::*;

const AUTHORITY_ORDER: &[&str] = &[
    "github_active_state",
    "linked_docs_specs",
    "verification_evidence",
    "runtime_artifacts",
    "memory_checkpoints",
    "wiki_distillation",
];

pub(crate) async fn handle_task_cycle_status(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    let started = std::time::Instant::now();
    let requested_issue_ref = normalize_optional_issue_ref(params.issue_ref.as_deref());
    let requested_pr_ref = normalize_optional_pr_ref(params.pr_ref.as_deref());
    let flow_id = params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .or_else(|| {
            find_flow_by_refs(requested_issue_ref.as_deref(), requested_pr_ref.as_deref()).ok()
        });
    if flow_id.is_none() && requested_issue_ref.is_none() && requested_pr_ref.is_none() {
        return Err(
            "cycle_status requires flow_id, issue_ref='owner/repo#123', or pr_ref='owner/repo#123' / GitHub PR URL"
                .to_string(),
        );
    }

    let run_dir = flow_id.as_deref().map(run_dir_for_flow_id).transpose()?;
    let status = run_dir
        .as_ref()
        .map(|dir| read_json_file(&dir.join("status.json")))
        .transpose()?
        .flatten()
        .unwrap_or_else(|| json!({}));
    let after_local = started.elapsed();
    let verification = flow_id
        .as_deref()
        .map(crate::verify_ops::read_verification_ledger)
        .transpose()?
        .flatten();
    let close_loop = run_dir
        .as_ref()
        .map(|dir| read_json_file(&dir.join("close_loop.json")))
        .transpose()?
        .flatten();
    let release_note_present = run_dir
        .as_ref()
        .is_some_and(|dir| dir.join("release_note.md").exists());
    let result_present = run_dir
        .as_ref()
        .is_some_and(|dir| dir.join("result.md").exists());
    let events = run_dir
        .as_ref()
        .map(|dir| read_events(&dir.join("events.jsonl")))
        .transpose()?
        .unwrap_or_default();

    let issue_ref = status_issue_ref(&status).or(requested_issue_ref.clone());
    let pr_ref = status_pr_ref(&status).or(requested_pr_ref.clone());
    let (issue_snapshot, issue_warning) = maybe_issue_snapshot(
        server,
        params,
        flow_id.as_deref(),
        issue_ref.as_deref(),
        &status,
    )
    .await;
    let (pr_snapshot, pr_warning) =
        maybe_pr_snapshot(server, flow_id.as_deref(), pr_ref.as_deref(), &status).await;
    let after_github = started.elapsed();

    let mut linked_docs = params.doc_paths.clone();
    let mut linked_specs = params.spec_paths.clone();
    linked_docs.extend(string_array_field(&status, "doc_paths"));
    linked_specs.extend(string_array_field(&status, "spec_paths"));
    if let Some(github) = status.get("github") {
        linked_docs.extend(string_array_field(github, "doc_paths"));
        linked_specs.extend(string_array_field(github, "spec_paths"));
    }
    if let Some(issue) = issue_snapshot.as_ref() {
        linked_docs.extend(string_array_field(issue, "doc_paths"));
        linked_specs.extend(string_array_field(issue, "spec_paths"));
    }
    dedupe_strings(&mut linked_docs);
    dedupe_strings(&mut linked_specs);

    let artifacts = artifacts_json(run_dir.as_ref(), verification.as_ref(), close_loop.as_ref());
    let merge_state = status
        .get("github")
        .and_then(|github| github.get("merge_state"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let stage = infer_cycle_stage(
        &status,
        issue_ref.as_deref(),
        pr_ref.as_deref(),
        verification.as_ref(),
        release_note_present,
        close_loop.as_ref(),
    );
    let drift = spec_drift(
        flow_id.as_deref(),
        requested_issue_ref.as_deref(),
        requested_pr_ref.as_deref(),
        issue_ref.as_deref(),
        pr_ref.as_deref(),
        &linked_docs,
        &linked_specs,
        verification.as_ref(),
        &status,
        result_present,
        release_note_present,
        close_loop.as_ref(),
        merge_state.as_deref(),
    );
    let warnings = [issue_warning, pr_warning]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let next_action = next_action(
        issue_ref.as_deref(),
        pr_ref.as_deref(),
        &linked_docs,
        &linked_specs,
        verification.as_ref(),
        merge_state.as_deref(),
        release_note_present,
        close_loop.as_ref(),
    );
    let github_read_attempted =
        flow_id.is_none() && (params.issue_ref.is_some() || params.pr_ref.is_some());

    serde_json::to_string(&json!({
        "ok": true,
        "action": "cycle_status",
        "cycle_id": flow_id.clone(),
        "flow_id": flow_id.clone(),
        "stage": stage,
        "state": status.get("state").cloned().unwrap_or(Value::Null),
        "issue_ref": issue_ref.clone(),
        "pr_ref": pr_ref.clone(),
        "linked_docs": linked_docs.clone(),
        "linked_specs": linked_specs.clone(),
        "contract_refs": {
            "docs": linked_docs.clone(),
            "specs": linked_specs.clone(),
            "authority": "linked_docs_specs",
        },
        "authority_order": AUTHORITY_ORDER,
        "github": {
            "cached": status.get("github").cloned().unwrap_or(Value::Null),
            "issue_snapshot": issue_snapshot,
            "pr_snapshot": pr_snapshot,
            "merge_state": merge_state,
        },
        "verification": verification,
        "artifacts": artifacts,
        "events": events,
        "spec_drift": drift,
        "warnings": warnings,
        "next_action": next_action,
        "source": {
            "flow_artifacts": run_dir.as_ref().map(|dir| dir.display().to_string()),
            "read_only": true,
            "github_read": github_read_attempted,
        },
        // #925: phase timings so agents can tell local ledger vs GitHub enrichment cost.
        "timing_ms": {
            "local_artifacts": after_local.as_millis() as u64,
            "github_enrichment": after_github.saturating_sub(after_local).as_millis() as u64,
            "total": started.elapsed().as_millis() as u64,
        },
    }))
    .map_err(|e| format!("serialize cycle_status: {e}"))
}

fn normalize_optional_issue_ref(raw: Option<&str>) -> Option<String> {
    raw.and_then(|value| parse_issue_ref(value, None))
        .map(|target| format!("{}#{}", target.repo, target.number))
}

fn normalize_optional_pr_ref(raw: Option<&str>) -> Option<String> {
    raw.and_then(parse_pr_ref)
        .map(|target| format!("{}#{}", target.repo, target.number))
}

fn find_flow_by_refs(issue_ref: Option<&str>, pr_ref: Option<&str>) -> Result<String, String> {
    let runs_root = crate::task_lifecycle::shell_runs_root();
    let read_dir = std::fs::read_dir(&runs_root)
        .map_err(|e| format!("read runs root {}: {e}", runs_root.display()))?;
    let mut matches = Vec::new();
    for entry in read_dir.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(status) = read_json_file(&dir.join("status.json"))? else {
            continue;
        };
        let status_issue = status_issue_ref(&status);
        let status_pr = status_pr_ref(&status);
        let issue_matches = issue_ref
            .zip(status_issue.as_deref())
            .is_some_and(|(requested, current)| requested == current);
        let pr_matches = pr_ref
            .zip(status_pr.as_deref())
            .is_some_and(|(requested, current)| requested == current);
        if !issue_matches && !pr_matches {
            continue;
        }
        let modified = dir
            .join("status.json")
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        matches.push((modified, entry.file_name().to_string_lossy().to_string()));
    }
    matches.sort_by(|a, b| b.0.cmp(&a.0));
    matches
        .into_iter()
        .next()
        .map(|(_, flow_id)| flow_id)
        .ok_or_else(|| "no matching local flow".to_string())
}

fn status_issue_ref(status: &Value) -> Option<String> {
    status_string(status, "issue_ref")
        .or_else(|| status_github_string(status, "issue_ref"))
        .or_else(|| {
            let github = status.get("github")?;
            let repo = github.get("repo").and_then(Value::as_str)?;
            let number = github.get("issue_number").and_then(Value::as_u64)?;
            Some(format!("{repo}#{number}"))
        })
}

fn status_pr_ref(status: &Value) -> Option<String> {
    status_string(status, "pr_ref")
        .or_else(|| status_github_string(status, "pr_ref"))
        .or_else(|| {
            let github = status.get("github")?;
            let repo = github.get("repo").and_then(Value::as_str)?;
            let number = github.get("pr_number").and_then(Value::as_u64)?;
            Some(format!("{repo}#{number}"))
        })
}

fn status_github_string(status: &Value, key: &str) -> Option<String> {
    status
        .get("github")
        .and_then(|github| github.get(key))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

async fn maybe_issue_snapshot(
    server: &MemoryServer,
    params: &TachiTaskParams,
    flow_id: Option<&str>,
    issue_ref: Option<&str>,
    status: &Value,
) -> (Option<Value>, Option<String>) {
    if flow_id.is_some() {
        return (cached_issue_snapshot(status), None);
    }
    let Some(target) = issue_ref.and_then(|value| parse_issue_ref(value, None)) else {
        return (cached_issue_snapshot(status), None);
    };
    match read_issue_snapshot(server, &target, params).await {
        Ok(issue) => (Some(issue_to_json(&issue)), None),
        Err(err) => (
            cached_issue_snapshot(status),
            Some(format!("github_issue_read_failed: {err}")),
        ),
    }
}

async fn maybe_pr_snapshot(
    server: &MemoryServer,
    flow_id: Option<&str>,
    pr_ref: Option<&str>,
    status: &Value,
) -> (Option<Value>, Option<String>) {
    if flow_id.is_some() {
        return (cached_pr_snapshot(status), None);
    }
    let Some(target) = pr_ref.and_then(parse_pr_ref) else {
        return (cached_pr_snapshot(status), None);
    };
    match read_pr_snapshot(server, &target).await {
        Ok(pr) => (Some(pr_to_json(&pr)), None),
        Err(err) => (
            cached_pr_snapshot(status),
            Some(format!("github_pr_read_failed: {err}")),
        ),
    }
}

fn cached_issue_snapshot(status: &Value) -> Option<Value> {
    let github = status.get("github")?;
    let repo = github.get("repo").and_then(Value::as_str)?;
    let number = github.get("issue_number").and_then(Value::as_u64)?;
    Some(json!({
        "repo": repo,
        "number": number,
        "title": github.get("issue_title").cloned().unwrap_or(Value::Null),
        "state": github.get("issue_state").cloned().unwrap_or(Value::Null),
        "url": github.get("issue_url").cloned().unwrap_or_else(|| json!(format!("https://github.com/{repo}/issues/{number}"))),
        "labels": github.get("labels").cloned().unwrap_or_else(|| json!([])),
        "doc_paths": github.get("doc_paths").cloned().unwrap_or_else(|| json!([])),
        "spec_paths": github.get("spec_paths").cloned().unwrap_or_else(|| json!([])),
        "source": "flow_status",
    }))
}

fn cached_pr_snapshot(status: &Value) -> Option<Value> {
    let github = status.get("github")?;
    let repo = github.get("repo").and_then(Value::as_str)?;
    let number = github.get("pr_number").and_then(Value::as_u64)?;
    Some(json!({
        "repo": repo,
        "number": number,
        "title": github.get("pr_title").cloned().unwrap_or(Value::Null),
        "state": github.get("pr_state").cloned().unwrap_or(Value::Null),
        "url": github.get("pr_url").cloned().unwrap_or_else(|| json!(format!("https://github.com/{repo}/pull/{number}"))),
        "head_ref": github.get("head_ref").cloned().unwrap_or(Value::Null),
        "base_ref": github.get("base_ref").cloned().unwrap_or(Value::Null),
        "review_decision": github
            .get("review")
            .and_then(|review| review.get("state"))
            .cloned()
            .unwrap_or(Value::Null),
        "mergeable": github.get("mergeable").cloned().unwrap_or(Value::Null),
        "source": "flow_status",
    }))
}

fn read_events(path: &Path) -> Result<Vec<Value>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(format!("read {}: {err}", path.display())),
    };
    let mut events = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(event) => events.push(event),
            Err(err) => events.push(json!({
                "kind": "event_parse_error",
                "line": idx + 1,
                "error": err.to_string(),
            })),
        }
    }
    let keep_from = events.len().saturating_sub(20);
    Ok(events.into_iter().skip(keep_from).collect())
}

fn artifacts_json(
    run_dir: Option<&PathBuf>,
    verification: Option<&Value>,
    close_loop: Option<&Value>,
) -> Value {
    let Some(run_dir) = run_dir else {
        return json!({
            "run_dir": null,
            "instruction": null,
            "pr_handoff": null,
            "verification": verification,
            "release_note": null,
            "close_loop": close_loop,
        });
    };
    json!({
        "run_dir": run_dir.display().to_string(),
        "instruction": artifact_entry(run_dir, "instruction.md"),
        "pr_handoff": artifact_entry(run_dir, "pr_handoff.md"),
        "verification": artifact_entry(run_dir, "verification.json"),
        "release_note": artifact_entry(run_dir, "release_note.md"),
        "close_loop": artifact_entry(run_dir, "close_loop.json"),
    })
}

fn artifact_entry(run_dir: &Path, file_name: &str) -> Value {
    let path = run_dir.join(file_name);
    json!({
        "path": path.display().to_string(),
        "exists": path.exists(),
    })
}

fn infer_cycle_stage(
    status: &Value,
    issue_ref: Option<&str>,
    pr_ref: Option<&str>,
    verification: Option<&Value>,
    release_note_present: bool,
    close_loop: Option<&Value>,
) -> String {
    if close_loop.is_some() {
        return "closed".to_string();
    }
    if release_note_present {
        return "release".to_string();
    }
    if let Some(overall) = verification
        .and_then(|ledger| ledger.get("overall"))
        .and_then(Value::as_str)
    {
        if overall == "passed" {
            return "verified".to_string();
        }
        return "verify".to_string();
    }
    if pr_ref.is_some() {
        return "review".to_string();
    }
    status_string(status, "stage").unwrap_or_else(|| {
        if issue_ref.is_some() {
            "intake".to_string()
        } else {
            "unbound".to_string()
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn spec_drift(
    flow_id: Option<&str>,
    requested_issue_ref: Option<&str>,
    requested_pr_ref: Option<&str>,
    issue_ref: Option<&str>,
    pr_ref: Option<&str>,
    linked_docs: &[String],
    linked_specs: &[String],
    verification: Option<&Value>,
    status: &Value,
    result_present: bool,
    release_note_present: bool,
    close_loop: Option<&Value>,
    merge_state: Option<&str>,
) -> Vec<Value> {
    let mut drift = Vec::new();
    if flow_id.is_none() {
        drift.push(drift_item(
            "no_local_flow",
            "No local Tachi flow was found for this issue/PR; status is based on refs and live/cached snapshots only.",
            "Run tachi_task(action='intake', issue_ref=...) to bind the work to a lifecycle flow.",
        ));
    }
    if issue_ref.is_none() {
        drift.push(drift_item(
            "missing_issue_ref",
            "Cycle has no linked GitHub issue, so requirements and acceptance criteria are not anchored.",
            "Run tachi_task(action='intake', issue_ref=...) or provide issue_ref.",
        ));
    }
    if let (Some(requested), Some(current)) = (requested_issue_ref, issue_ref) {
        if requested != current {
            drift.push(drift_item(
                "issue_ref_mismatch",
                &format!("Requested issue_ref {requested} differs from flow issue_ref {current}."),
                "Use the flow's issue_ref or start a separate flow for the requested issue.",
            ));
        }
    }
    if let (Some(requested), Some(current)) = (requested_pr_ref, pr_ref) {
        if requested != current {
            drift.push(drift_item(
                "pr_ref_mismatch",
                &format!("Requested pr_ref {requested} differs from flow pr_ref {current}."),
                "Use the flow's pr_ref or link the intended PR with tachi_gh(action='link_pr').",
            ));
        }
    }
    if linked_docs.is_empty() && linked_specs.is_empty() {
        drift.push(drift_item(
            "missing_docs_specs",
            "Cycle has no linked docs/specs; memory would become the only design context.",
            "Attach doc_paths or spec_paths before treating the cycle as contract-backed.",
        ));
    } else if linked_specs.is_empty() {
        drift.push(drift_item(
            "missing_linked_specs",
            "Cycle has linked docs but no explicit spec refs.",
            "Add spec_paths for contract-level behavior or confirm the linked docs are sufficient.",
        ));
    }
    if pr_ref.is_some() && verification.is_none() {
        drift.push(drift_item(
            "missing_verification",
            "Cycle has a linked PR but no Tachi verification ledger.",
            "Record required checks with tachi_verify before safe_merge or close_loop.",
        ));
    }
    if let Some(verification) = verification {
        if let Some(overall) = verification.get("overall").and_then(Value::as_str) {
            if overall != "passed" {
                drift.push(drift_item(
                    "verification_not_passed",
                    &format!("Verification ledger overall state is {overall}."),
                    "Resolve pending/failed/stale checks before merge or close_loop.",
                ));
            }
        }
        let verification_head = verification.get("head_sha").and_then(Value::as_str);
        let github_head = status
            .get("github")
            .and_then(|github| github.get("head_sha"))
            .and_then(Value::as_str);
        if let (Some(v_head), Some(g_head)) = (verification_head, github_head) {
            if v_head != g_head {
                drift.push(drift_item(
                    "verification_head_mismatch",
                    &format!(
                        "Verification head_sha {v_head} differs from GitHub PR head_sha {g_head}."
                    ),
                    "Re-run required verification on the current PR head.",
                ));
            }
        }
    }
    if result_present && close_loop.is_none() && issue_ref.is_some() {
        drift.push(drift_item(
            "unclosed_loop",
            "Flow produced result.md but close_loop has not recorded issue/docs/wiki closure.",
            "Run tachi_task(action='close_loop', flow_id=..., issue_ref=..., pr_ref=...).",
        ));
    }
    if close_loop.is_some() && linked_specs.is_empty() {
        drift.push(drift_item(
            "closed_without_specs",
            "Cycle is closed but has no linked spec refs.",
            "Re-run close_loop with spec_paths or update the canonical docs/spec refs.",
        ));
    }
    if release_note_present && !matches!(merge_state, Some("ready" | "merged")) {
        drift.push(drift_item(
            "release_note_before_ready_pr",
            "Release note exists but PR merge state is not ready or merged.",
            "Run tachi_gh(action='pr_status', flow_id=..., pr_ref=...) and resolve the PR gate.",
        ));
    }
    drift
}

fn drift_item(kind: &str, detail: &str, action: &str) -> Value {
    json!({
        "kind": kind,
        "detail": detail,
        "action": action,
        "authority": "cycle_status",
    })
}

fn next_action(
    issue_ref: Option<&str>,
    pr_ref: Option<&str>,
    linked_docs: &[String],
    linked_specs: &[String],
    verification: Option<&Value>,
    merge_state: Option<&str>,
    release_note_present: bool,
    close_loop: Option<&Value>,
) -> String {
    if issue_ref.is_none() {
        return "Run tachi_task(action='intake', issue_ref=...) to anchor requirements."
            .to_string();
    }
    if linked_docs.is_empty() && linked_specs.is_empty() {
        return "Attach linked docs/specs through doc_paths/spec_paths before execution."
            .to_string();
    }
    if linked_specs.is_empty() {
        return "Attach linked spec refs or confirm docs are the contract source.".to_string();
    }
    if pr_ref.is_none() {
        return "Prepare or link a PR with tachi_gh(action='pr_handoff') and tachi_gh(action='link_pr')."
            .to_string();
    }
    match verification
        .and_then(|ledger| ledger.get("overall"))
        .and_then(Value::as_str)
    {
        None => {
            return "Record required verification with tachi_verify before PR gate.".to_string()
        }
        Some("passed") => {}
        Some(other) => return format!("Resolve verification state `{other}` before PR gate."),
    }
    if !matches!(merge_state, Some("ready" | "merged")) {
        return "Run tachi_gh(action='pr_status', flow_id=..., pr_ref=...) and resolve PR gate."
            .to_string();
    }
    if !release_note_present {
        return "Run tachi_gh(action='release_note', flow_id=...) after PR gate is ready/merged."
            .to_string();
    }
    if close_loop.is_none() {
        return "Run tachi_task(action='close_loop', flow_id=..., issue_ref=..., pr_ref=...) to sink lessons."
            .to_string();
    }
    "Cycle is closed; distill durable lessons only if new reusable patterns emerged.".to_string()
}
