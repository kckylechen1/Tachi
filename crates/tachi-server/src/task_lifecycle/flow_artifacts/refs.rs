use super::*;

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

pub(in crate::task_lifecycle) fn normalize_issue_ref(raw: &str) -> Result<String, String> {
    parse_issue_ref(raw, None)
        .map(|target| format!("{}#{}", target.repo, target.number))
        .ok_or_else(|| {
            "issue_ref must be owner/repo#123 or a GitHub issue URL for link_pr".to_string()
        })
}

pub(in crate::task_lifecycle) fn initial_merge_state_for_pr(state: Option<&str>) -> &'static str {
    match state.unwrap_or("").to_ascii_uppercase().as_str() {
        "MERGED" => "merged",
        "CLOSED" => "blocked",
        _ => "pending",
    }
}
