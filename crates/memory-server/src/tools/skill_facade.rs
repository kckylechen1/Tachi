use super::*;

pub(super) async fn handle_tachi_skill_facade(
    server: &MemoryServer,
    params: TachiSkillParams,
) -> Result<String, String> {
    let action = params.action.to_ascii_lowercase();
    match action.as_str() {
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
            serde_json::to_string(&json!({
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
        "bundle" => {
            let query = required_skill_query(&params, "bundle")?;
            let bundle_params = skill_bundle_params(&params, query, params.host.clone());
            handle_prepare_capability_bundle(server, bundle_params).await
        }
        "loadout" => {
            let profile_name = params
                .profile
                .as_deref()
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
                .ok_or_else(|| "profile is required when action='loadout'".to_string())?;
            let profile = crate::dispatch_profile::resolve_dispatch_profile(profile_name)
                .ok_or_else(|| {
                    format!(
                        "Unknown dispatch profile '{}'. Supported: {}",
                        profile_name,
                        crate::dispatch_profile::DISPATCH_PROFILES
                            .iter()
                            .map(|profile| profile.name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
            let query = params.query.clone().unwrap_or_else(|| {
                format!(
                    "{} {} {}",
                    profile.role,
                    profile.common_skills.join(" "),
                    profile.signature_skills.join(" ")
                )
            });
            let host = params
                .host
                .clone()
                .or_else(|| Some(profile.backend.to_string()));
            let bundle_raw = handle_prepare_capability_bundle(
                server,
                skill_bundle_params(&params, query.clone(), host.clone()),
            )
            .await?;
            let bundle_value: Value = serde_json::from_str(&bundle_raw)
                .map_err(|e| format!("parse capability bundle: {e}"))?;
            serde_json::to_string(&json!({
                "action": "loadout",
                "profile": profile.name,
                "display_name": profile.display_name,
                "role": profile.role,
                "stage": profile.stage,
                "backend": profile.backend,
                "host": host,
                "resolved_skills": crate::dispatch_profile::profile_required_skill_ids_for_server(server, profile)?,
                "skill_loadout": crate::dispatch_profile::profile_skill_loadout_json_for_server(server, profile)?,
                "evidence_required": crate::dispatch_profile::profile_evidence_required_for_server(server, profile)?,
                "evidence_contract": crate::dispatch_profile::profile_evidence_contract_json_for_server(server, profile)?,
                "strong_against": profile.strong_against,
                "weak_against": crate::dispatch_profile::profile_weak_against_for_server(server, profile)?,
                "auto_capability_bundle": profile.auto_capability_bundle,
                "capability_bundle": bundle_value.get("bundle").cloned().unwrap_or(Value::Null),
                "eval_feedback": crate::dispatch_profile::profile_eval_feedback_json(server, profile, params.limit.unwrap_or(500))?,
                "mbit_card": crate::dispatch_profile::profile_json_for_server(server, profile)?.get("mbit_card").cloned().unwrap_or(Value::Null),
            }))
            .map_err(|e| format!("serialize skill loadout: {e}"))
        }
        _ => Err(format!(
            "Invalid action '{}'. Use 'discover', 'bundle', 'loadout', or 'run'.",
            params.action
        )),
    }
}
