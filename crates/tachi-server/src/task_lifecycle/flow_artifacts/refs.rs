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

#[cfg(test)]
pub(in crate::task_lifecycle) fn initial_merge_state_for_pr(state: Option<&str>) -> &'static str {
    match state.unwrap_or("").to_ascii_uppercase().as_str() {
        "MERGED" => "merged",
        "CLOSED" => "blocked",
        _ => "pending",
    }
}
