use super::*;

pub(super) fn issue_to_json(issue: &IssueSnapshot) -> Value {
    json!({
        "repo": issue.repo,
        "number": issue.number,
        "title": issue.title,
        "body": issue.body,
        "labels": issue.labels,
        "state": issue.state,
        "url": issue.url,
        "doc_paths": issue.doc_paths,
        "spec_paths": issue.spec_paths,
    })
}

pub(super) fn pr_to_json(pr: &PrSnapshot) -> Value {
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

pub(super) fn ux_step(
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

pub(super) fn vec_if<const N: usize>(items: [(bool, &'static str); N]) -> Vec<String> {
    items
        .into_iter()
        .filter_map(|(include, message)| include.then_some(message.to_string()))
        .collect()
}

pub(super) fn gaps_if<const N: usize>(items: [(bool, &'static str); N]) -> Vec<String> {
    // Semantic alias for UX matrix call sites: evidence and gaps share shape.
    vec_if(items)
}

pub(super) fn status_string(status: &Value, key: &str) -> Option<String> {
    status
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

pub(super) fn write_intake_instruction(
    run_dir: &Path,
    flow_id: &str,
    objective: &str,
    issue: &IssueSnapshot,
    automation_plan: &Value,
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
    if !issue.labels.is_empty() {
        body.push_str(&format!("- labels: `{}`\n", issue.labels.join("`, `")));
    }
    body.push_str("\n## Automation Gate\n\n");
    body.push_str(&format!(
        "- status: `{}`\n",
        automation_plan
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    ));
    body.push_str(&format!(
        "- dispatch_allowed: `{}`\n",
        automation_plan
            .get("dispatch_allowed")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    ));
    let gate_reasons = string_array_field(automation_plan, "leader_gate_reasons");
    if !gate_reasons.is_empty() {
        body.push_str("- leader_gate_reasons:\n");
        for reason in gate_reasons {
            body.push_str(&format!("  - `{reason}`\n"));
        }
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
    body.push_str("- `tachi_task(action='cycle_plan', flow_id=...)`\n");
    body.push_str("- `tachi_task(action='briefing', flow_id=...)`\n");
    body.push_str("- `tachi_task(action='recommend', task=..., doc_paths=[...])`\n");
    body.push_str("- `tachi_task(action='dispatch', flow_id=..., issue_ref=...)`\n");
    body.push_str("- `tachi_gh(action='pr_handoff', flow_id=...)`\n");
    body.push_str("- `tachi_gh(action='link_pr', flow_id=..., pr_ref=...)`\n");
    body.push_str("- `tachi_gh(action='pr_status', flow_id=..., pr_ref=...)`\n");
    write_text_atomic(&run_dir.join("instruction.md"), &body)
        .map_err(|e| format!("write intake instruction.md: {e}"))
}

pub(super) fn merge_flow_status(run_dir: &Path, patch: Value) -> Result<Value, String> {
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

pub(super) fn write_json_atomic(path: &Path, value: &Value) -> Result<(), String> {
    let serialized = serde_json::to_string_pretty(value)
        .map_err(|e| format!("serialize {}: {e}", path.display()))?;
    crate::utils::write_owner_only_file_atomic(path, serialized.as_bytes())
}

pub(super) fn write_text_atomic(path: &Path, body: &str) -> Result<(), String> {
    crate::utils::write_owner_only_file_atomic(path, body.as_bytes())
}

pub(super) fn append_flow_event(run_dir: &Path, event: Value) -> Result<(), String> {
    let line = serde_json::to_string(&event).map_err(|e| format!("serialize flow event: {e}"))?;
    crate::utils::append_owner_only_jsonl_line(&run_dir.join("events.jsonl"), &line)
}

pub(super) fn deep_merge(target: &mut Value, patch: Value) {
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

pub(super) fn string_array_field(value: &Value, field: &str) -> Vec<String> {
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

pub(super) fn extract_markdown_paths(text: &str) -> Vec<String> {
    text.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ')' | '(' | '[' | ']'))
        .map(|token| token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | ':' | ';')))
        .filter(|token| {
            token.ends_with(".md") && (token.starts_with("docs/") || token.contains("/docs/"))
        })
        .map(str::to_string)
        .collect()
}

pub(super) fn dedupe_strings(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

pub(super) fn new_task_flow_id(stage: &str, title: &str) -> String {
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
