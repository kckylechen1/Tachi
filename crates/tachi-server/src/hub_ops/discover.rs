use crate::tool_params::{HubDiscoverParams, HubFeedbackParams, HubGetParams};
use crate::utils::redact_sensitive_value;
use crate::MemoryServer;
use memcore::HubCapability;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use tachi_hub::{capability_callable, capability_visibility_for_cap};

fn hub_capability_is_callable(cap: &HubCapability) -> bool {
    !crate::builtins::is_retired_builtin_capability_id(&cap.id) && capability_callable(cap)
}

pub(crate) async fn handle_hub_discover(
    server: &MemoryServer,
    params: HubDiscoverParams,
) -> Result<String, String> {
    let output = hub_discover_inner(server, &params)?;
    serde_json::to_string(&output).map_err(|e| format!("serialize: {e}"))
}

/// Shared inner function returning structured data to avoid double serde round-trips.
pub(super) fn hub_discover_inner(
    server: &MemoryServer,
    params: &HubDiscoverParams,
) -> Result<Vec<serde_json::Value>, String> {
    let cap_type = params.cap_type.as_deref();

    let global_caps = server.with_global_store(|store| {
        if let Some(ref q) = params.query {
            store
                .hub_search(q, cap_type)
                .map_err(|e| format!("hub search global: {e}"))
        } else {
            store
                .hub_list(cap_type, params.enabled_only)
                .map_err(|e| format!("hub list global: {e}"))
        }
    })?;

    let project_caps = if server.has_project_db() {
        server.with_project_store(|store| {
            if let Some(ref q) = params.query {
                store
                    .hub_search(q, cap_type)
                    .map_err(|e| format!("hub search project: {e}"))
            } else {
                store
                    .hub_list(cap_type, params.enabled_only)
                    .map_err(|e| format!("hub list project: {e}"))
            }
        })?
    } else {
        vec![]
    };

    let mut seen = HashSet::new();
    let mut output: Vec<serde_json::Value> = Vec::new();

    for cap in &project_caps {
        if crate::builtins::is_retired_builtin_capability_id(&cap.id) {
            continue;
        }
        if params.enabled_only && !cap.enabled {
            continue;
        }
        seen.insert(cap.id.clone());
        let mut obj = serde_json::to_value(cap).unwrap_or(json!(null));
        if let Some(o) = obj.as_object_mut() {
            o.insert("db".into(), json!("project"));
            o.insert(
                "visibility".into(),
                json!(capability_visibility_for_cap(cap).as_str()),
            );
            o.insert("callable".into(), json!(hub_capability_is_callable(cap)));
        }
        redact_sensitive_value(&mut obj);
        output.push(obj);
    }
    for cap in &global_caps {
        if crate::builtins::is_retired_builtin_capability_id(&cap.id) {
            continue;
        }
        if params.enabled_only && !cap.enabled {
            continue;
        }
        if !seen.insert(cap.id.clone()) {
            continue;
        }
        let mut obj = serde_json::to_value(cap).unwrap_or(json!(null));
        if let Some(o) = obj.as_object_mut() {
            o.insert("db".into(), json!("global"));
            o.insert(
                "visibility".into(),
                json!(capability_visibility_for_cap(cap).as_str()),
            );
            o.insert("callable".into(), json!(hub_capability_is_callable(cap)));
        }
        redact_sensitive_value(&mut obj);
        output.push(obj);
    }

    Ok(output)
}

pub(crate) async fn handle_hub_get(
    server: &MemoryServer,
    params: HubGetParams,
) -> Result<String, String> {
    if server.has_project_db() {
        if let Some(cap) = server.with_project_store(|store| {
            store
                .hub_get(&params.id)
                .map_err(|e| format!("hub get project: {e}"))
        })? {
            let mut obj = serde_json::to_value(&cap).unwrap_or(json!(null));
            if let Some(o) = obj.as_object_mut() {
                if crate::builtins::is_retired_builtin_capability_id(&cap.id) {
                    o.insert("enabled".into(), json!(false));
                    o.insert("review_status".into(), json!("rejected"));
                }
                o.insert("db".into(), json!("project"));
                o.insert(
                    "visibility".into(),
                    json!(capability_visibility_for_cap(&cap).as_str()),
                );
                o.insert("callable".into(), json!(hub_capability_is_callable(&cap)));
            }
            redact_sensitive_value(&mut obj);
            return serde_json::to_string(&obj).map_err(|e| format!("serialize: {e}"));
        }
    }
    match server.with_global_store(|store| {
        store
            .hub_get(&params.id)
            .map_err(|e| format!("hub get global: {e}"))
    })? {
        Some(cap) => {
            let mut obj = serde_json::to_value(&cap).unwrap_or(json!(null));
            if let Some(o) = obj.as_object_mut() {
                if crate::builtins::is_retired_builtin_capability_id(&cap.id) {
                    o.insert("enabled".into(), json!(false));
                    o.insert("review_status".into(), json!("rejected"));
                }
                o.insert("db".into(), json!("global"));
                o.insert(
                    "visibility".into(),
                    json!(capability_visibility_for_cap(&cap).as_str()),
                );
                o.insert("callable".into(), json!(hub_capability_is_callable(&cap)));
            }
            redact_sensitive_value(&mut obj);
            serde_json::to_string(&obj).map_err(|e| format!("serialize: {e}"))
        }
        None => serde_json::to_string(&json!({"error": "Capability not found"}))
            .map_err(|e| format!("serialize: {e}")),
    }
}

pub(crate) async fn handle_hub_feedback(
    server: &MemoryServer,
    params: HubFeedbackParams,
) -> Result<String, String> {
    if crate::builtins::is_retired_builtin_capability_id(&params.id) {
        return Err(format!(
            "Capability '{}' is retired and cannot accept feedback.",
            params.id
        ));
    }
    if server.has_project_db() {
        let found = server.with_project_store(|store| {
            store
                .hub_get(&params.id)
                .map_err(|e| format!("hub get: {e}"))
        })?;
        if found.is_some() {
            let recorded = server.with_project_store(|store| {
                store
                    .hub_record_feedback(&params.id, params.success, params.rating)
                    .map_err(|e| format!("feedback: {e}"))
            })?;
            if recorded {
                let _ = crate::wiki_ops::refresh_skill_quality_guards(server);
            }
            return serde_json::to_string(
                &json!({"id": params.id, "recorded": recorded, "db": "project"}),
            )
            .map_err(|e| format!("serialize: {e}"));
        }
    }
    let recorded = server.with_global_store(|store| {
        store
            .hub_record_feedback(&params.id, params.success, params.rating)
            .map_err(|e| format!("feedback: {e}"))
    })?;
    if recorded {
        let _ = crate::wiki_ops::refresh_skill_quality_guards(server);
    }
    serde_json::to_string(&json!({"id": params.id, "recorded": recorded, "db": "global"}))
        .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_hub_stats(server: &MemoryServer) -> Result<String, String> {
    let global_caps = server.with_global_store(|store| {
        store
            .hub_list(None, false)
            .map_err(|e| format!("hub list: {e}"))
    })?;
    let project_caps = if server.has_project_db() {
        server.with_project_store(|store| {
            store
                .hub_list(None, false)
                .map_err(|e| format!("hub list: {e}"))
        })?
    } else {
        vec![]
    };

    let total = global_caps.len() + project_caps.len();
    let mut by_type: HashMap<String, usize> = HashMap::new();
    let all_caps: Vec<&HubCapability> = global_caps.iter().chain(project_caps.iter()).collect();
    for cap in &all_caps {
        *by_type.entry(cap.cap_type.clone()).or_insert(0) += 1;
    }
    let total_uses: u64 = all_caps.iter().map(|c| c.uses).sum();
    let total_successes: u64 = all_caps.iter().map(|c| c.successes).sum();

    serde_json::to_string(&json!({
        "total_capabilities": total,
        "by_type": by_type,
        "total_uses": total_uses,
        "total_successes": total_successes,
        "success_rate": if total_uses > 0 { total_successes as f64 / total_uses as f64 } else { 0.0 },
        "global_count": global_caps.len(),
        "project_count": project_caps.len(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}
