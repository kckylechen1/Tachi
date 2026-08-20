use super::*;

pub(super) async fn handle_tachi_skill_facade(
    server: &MemoryServer,
    params: TachiSkillParams,
) -> Result<String, String> {
    let action = params.action.to_ascii_lowercase();
    reject_delegate_skill_action(server, &action)?;
    let raw = match action.as_str() {
        "discover" => {
            let discover_params = HubDiscoverParams {
                query: params.query.clone(),
                cap_type: params
                    .cap_type
                    .clone()
                    .or_else(|| Some("skill".to_string())),
                enabled_only: params.enabled_only.unwrap_or(true),
            };
            let raw = handle_hub_discover(server, discover_params).await?;
            let mut capabilities: Vec<Value> =
                serde_json::from_str(&raw).map_err(|e| format!("parse hub discover: {e}"))?;
            if params.enabled_only.unwrap_or(true) {
                capabilities.retain(skill_discover_result_is_callable);
            }
            let limit = params.limit.unwrap_or(10).max(1);
            capabilities.truncate(limit);
            let mut results = capabilities
                .into_iter()
                .map(|cap| {
                    json!({
                        "id": cap.get("id").cloned().unwrap_or(Value::Null),
                        "name": cap.get("name").cloned().unwrap_or(Value::Null),
                        "description": cap.get("description").cloned().unwrap_or(Value::Null),
                        "cap_type": cap.get("cap_type").cloned().unwrap_or_else(|| cap.get("type").cloned().unwrap_or(Value::Null)),
                        "enabled": cap.get("enabled").cloned().unwrap_or(Value::Null),
                        "review_status": cap.get("review_status").cloned().unwrap_or(Value::Null),
                        "health_status": cap.get("health_status").cloned().unwrap_or(Value::Null),
                        "visibility": cap.get("visibility").cloned().unwrap_or(Value::Null),
                        "callable": cap.get("callable").cloned().unwrap_or(Value::Null),
                        "db": cap.get("db").cloned().unwrap_or(Value::Null),
                        "source": "local_approved_cache",
                    })
                })
                .collect::<Vec<_>>();
            if results.len() < limit {
                let mut local = discover_local_host_skills(
                    params.query.as_deref().unwrap_or_default(),
                    limit - results.len(),
                );
                let seen = results
                    .iter()
                    .filter_map(|cap| cap.get("id").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect::<std::collections::HashSet<_>>();
                let hub_skill_names = results
                    .iter()
                    .filter_map(canonical_skill_name)
                    .collect::<std::collections::HashSet<_>>();
                local.retain(|cap| {
                    let id_unseen = cap
                        .get("id")
                        .and_then(Value::as_str)
                        .is_none_or(|id| !seen.contains(id));
                    let name_unseen = canonical_skill_name(cap)
                        .is_none_or(|name| !hub_skill_names.contains(&name));
                    id_unseen && name_unseen
                });
                results.extend(local);
            }
            results.retain(|cap| {
                cap.get("id")
                    .and_then(Value::as_str)
                    .is_none_or(|id| !crate::builtins::is_retired_builtin_capability_id(id))
            });
            serde_json::to_string(&json!({
                "status": "completed",
                "action": "discover",
                "query": params.query,
                "search_backend": if params.query.is_some() { "hub_search+local_skill_index" } else { "hub_list+local_skill_index" },
                "online_search": false,
                "source": "local_approved_cache+host_skill_dirs",
                "count": results.len(),
                "results": results,
            }))
            .map_err(|e| format!("serialize skill discover: {e}"))
        }
        "run" => {
            let skill_id = params
                .skill_id
                .clone()
                .ok_or_else(|| "skill_id is required when action='run'".to_string())?;
            let run_params = RunSkillParams {
                skill_id,
                args: params.args.clone().unwrap_or(serde_json::Value::Null),
            };
            handle_run_skill(server, run_params).await
        }
        _ => Err(format!(
            "Invalid action '{}'. Use 'discover' or 'run'.",
            params.action
        )),
    }?;
    normalize_skill_response(&action, &raw)
}

/// Defense-in-depth for skill facade; primary gate is F3 `facade_action_allowed`
/// in `call_tool` (covers MCP path). Direct internal calls still hit this.
fn reject_delegate_skill_action(server: &MemoryServer, action: &str) -> Result<(), String> {
    if !tachi_hub::facade_action_allowed("tachi_skill", Some(action), server.active_tool_profile())
    {
        return Err(format!(
            "tachi_skill(action='{action}') is not available to the active tool profile; delegate workers may use 'discover' or 'run'."
        ));
    }
    Ok(())
}

fn normalize_skill_response(action: &str, raw: &str) -> Result<String, String> {
    let Ok(mut value) = serde_json::from_str::<Value>(raw) else {
        return Ok(raw.to_string());
    };
    if let Some(obj) = value.as_object_mut() {
        obj.entry("status".to_string())
            .or_insert_with(|| Value::String("completed".to_string()));
        obj.entry("action".to_string())
            .or_insert_with(|| Value::String(action.to_string()));
    }
    serde_json::to_string(&value)
        .map_err(|err| format!("serialize normalized skill response: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_skill_response_adds_missing_envelope_fields() {
        let raw = r#"{"results":[]}"#;
        let normalized = normalize_skill_response("discover", raw).unwrap();
        let value: Value = serde_json::from_str(&normalized).unwrap();
        assert_eq!(value["action"], "discover");
        assert_eq!(value["status"], "completed");
    }
}
