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

pub(super) fn suggested_feature_handoff(
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
    let mut context = serde_json::Map::new();
    context.insert(
        "task".to_string(),
        json!(params.task.as_deref().unwrap_or(query)),
    );
    if let Some(profile) = profile {
        context.insert("advisory_profile".to_string(), json!(profile));
    }
    if let Some(cwd) = params
        .cwd
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        context.insert("cwd".to_string(), json!(cwd));
    }
    if let Some(project) = params
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        context.insert("project".to_string(), json!(project));
    }
    if let Some(issue_ref) = params
        .issue_ref
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        context.insert("issue_ref".to_string(), json!(issue_ref));
    }
    if let Some(pr_ref) = params
        .pr_ref
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        context.insert("pr_ref".to_string(), json!(pr_ref));
    }
    if let Some(flow_id) = params
        .flow_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        context.insert("flow_id".to_string(), json!(flow_id));
    }
    if let Some(risk) = params
        .risk
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        context.insert("risk".to_string(), json!(risk));
    }
    if let Some(true) = params.auto_capability_bundle {
        context.insert("auto_capability_bundle".to_string(), json!(true));
    }
    json!({
        "mechanism": "harness_native_subagent",
        "context": context,
        "note": "Use the host harness's native subagent. The profile is advisory routing evidence, not authorization for Tachi worker dispatch.",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_params::TachiTaskParams;

    fn test_server() -> MemoryServer {
        let db_path = crate::utils::test_fixture_path(format!(
            "feature-briefing-dispatch-seam-a-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        MemoryServer::new(db_path, None).expect("test memory server")
    }

    /// tachi#1675 PR1 Seam A: `feature_dispatch_recommendation` is a SECOND
    /// call site into `handle_dispatch_recommendation` (the briefing
    /// plumbing, distinct from the direct `recommend` action) — it must be
    /// covered by the same `route_recommendations` write, not bypass it.
    #[test]
    fn feature_dispatch_recommendation_writes_a_route_recommendations_row() {
        let server = test_server();
        let before: i64 = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM route_recommendations", [], |r| {
                        r.get(0)
                    })
                    .map_err(|e| e.to_string())
            })
            .unwrap();

        let params: TachiTaskParams = serde_json::from_value(json!({"action": "briefing"}))
            .expect("minimal task params parse");
        let recommendation = feature_dispatch_recommendation(&server, &params, "fix a bug");
        assert_eq!(
            recommendation.get("available"),
            None,
            "a healthy recommendation carries no available:false error marker"
        );
        assert!(
            recommendation.get("recommendation_id").is_some(),
            "Seam A's recommendation_id must round-trip through the briefing plumbing too"
        );

        let after: i64 = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM route_recommendations", [], |r| {
                        r.get(0)
                    })
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        assert_eq!(after, before + 1);
    }
}
