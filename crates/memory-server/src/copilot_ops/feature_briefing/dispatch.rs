use super::super::*;

pub(super) fn feature_dispatch_recommendation(
    server: &MemoryServer,
    params: &TachiTaskParams,
    query: &str,
) -> Value {
    let mut file_paths = params.doc_paths.clone();
    file_paths.extend(params.spec_paths.clone());
    match crate::dispatch_profile::handle_dispatch_recommendation(
        server,
        query,
        params.risk.as_deref(),
        params.limit.unwrap_or(500),
        &file_paths,
    ) {
        Ok(raw) => serde_json::from_str(&raw)
            .unwrap_or_else(|err| json!({"available": false, "error": err.to_string()})),
        Err(err) => json!({"available": false, "error": err}),
    }
}

pub(super) fn suggested_feature_dispatch(
    params: &TachiTaskParams,
    query: &str,
    recommendation: &Value,
) -> Value {
    let profile = params.profile.as_deref().map(str::to_string).or_else(|| {
        recommendation
            .get("recommended_profile")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let mut arguments = serde_json::Map::new();
    arguments.insert("action".to_string(), json!("dispatch"));
    arguments.insert(
        "task".to_string(),
        json!(params.task.as_deref().unwrap_or(query)),
    );
    if let Some(profile) = profile {
        arguments.insert("profile".to_string(), json!(profile));
    }
    if let Some(cwd) = params
        .cwd
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("cwd".to_string(), json!(cwd));
    }
    if let Some(project) = params
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("project".to_string(), json!(project));
    }
    if let Some(issue_ref) = params
        .issue_ref
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("issue_ref".to_string(), json!(issue_ref));
    }
    if let Some(pr_ref) = params
        .pr_ref
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("pr_ref".to_string(), json!(pr_ref));
    }
    if let Some(flow_id) = params
        .flow_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("flow_id".to_string(), json!(flow_id));
    }
    if let Some(risk) = params
        .risk
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("risk".to_string(), json!(risk));
    }
    if let Some(true) = params.auto_capability_bundle {
        arguments.insert("auto_capability_bundle".to_string(), json!(true));
    }
    json!({
        "tool": "tachi_task",
        "arguments": arguments,
        "evidence_required": recommendation
            .get("evidence_required")
            .cloned()
            .unwrap_or(Value::Null),
        "fallback_chain": recommendation
            .get("fallback_chain")
            .cloned()
            .unwrap_or_else(|| json!([])),
    })
}

pub(super) fn relevant_feature_profiles(recommendation: &Value) -> Vec<Value> {
    recommendation
        .get("candidates")
        .and_then(Value::as_array)
        .map(|candidates| {
            candidates
                .iter()
                .take(4)
                .map(|candidate| {
                    json!({
                        "profile": candidate.get("profile").cloned().unwrap_or(Value::Null),
                        "agent": candidate.get("agent").cloned().unwrap_or(Value::Null),
                        "role": candidate.get("role").cloned().unwrap_or(Value::Null),
                        "score": candidate.get("score").cloned().unwrap_or(Value::Null),
                        "reason": candidate.get("reasons").cloned().unwrap_or_else(|| json!([])),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
