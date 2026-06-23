use super::status::{
    github_string, pr_snapshot_from_status, release_note_issue_ref, release_note_pr_ref,
    review_state,
};
use super::*;

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

pub(super) fn resolve_task_release_note_pr_target(
    params: &TachiTaskParams,
) -> Result<GithubTarget, String> {
    resolve_task_pr_target(params).map_err(|_| {
        "release_note requires flow_id or pr_ref='owner/repo#123' / GitHub PR URL".to_string()
    })
}

pub(super) fn reject_release_note_pr_mismatch(
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

pub(super) fn build_release_note_markdown(
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

pub(super) fn append_verification_summary(body: &mut String, verification: Option<&Value>) {
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
