use crate::tool_params::{HubReviewParams, HubSetEnabledParams};
use crate::MemoryServer;
use memcore::HubCapability;
use serde_json::json;
use tachi_hub::{capability_callable, normalize_review_status, should_expose_skill_tool};

pub(crate) async fn handle_hub_review(
    server: &MemoryServer,
    params: HubReviewParams,
) -> Result<String, String> {
    let review_status = normalize_review_status(&params.review_status)
        .ok_or_else(|| "review_status must be one of: pending, approved, rejected".to_string())?;
    if crate::builtins::is_retired_builtin_capability_id(&params.id)
        && (review_status != "rejected" || params.enabled == Some(true))
    {
        return Err(format!(
            "Capability '{}' is retired; only review_status='rejected' with enabled=false or omitted is allowed.",
            params.id
        ));
    }

    let enabled_override = params.enabled.or(match review_status {
        "approved" => Some(true),
        "rejected" => Some(false),
        _ => None,
    });

    let mut updated = false;
    let mut target_db = "not_found";
    let mut cap: Option<HubCapability> = None;

    if server.has_project_db() {
        let project_existing = server.with_project_store_read(|store| {
            store
                .hub_get(&params.id)
                .map_err(|e| format!("hub get project: {e}"))
        })?;
        if let Some(existing) = project_existing {
            if crate::builtins::is_retired_hub_capability(&existing)
                && (review_status != "rejected" || params.enabled == Some(true))
            {
                return Err(format!(
                    "Capability '{}' (type '{}') is retired; only review_status='rejected' with enabled=false or omitted is allowed.",
                    params.id, existing.cap_type
                ));
            }
            updated = server.with_project_store(|store| {
                store
                    .hub_set_review(&params.id, review_status, enabled_override)
                    .map_err(|e| format!("hub review project: {e}"))
            })?;
            if updated {
                target_db = "project";
                cap = server.with_project_store_read(|store| {
                    store
                        .hub_get(&params.id)
                        .map_err(|e| format!("hub get project: {e}"))
                })?;
            }
        }
    }

    if !updated {
        if let Some(existing) = server.with_global_store_read(|store| {
            store
                .hub_get(&params.id)
                .map_err(|e| format!("hub get global: {e}"))
        })? {
            if crate::builtins::is_retired_hub_capability(&existing)
                && (review_status != "rejected" || params.enabled == Some(true))
            {
                return Err(format!(
                    "Capability '{}' (type '{}') is retired; only review_status='rejected' with enabled=false or omitted is allowed.",
                    params.id, existing.cap_type
                ));
            }
        }
        updated = server.with_global_store(|store| {
            store
                .hub_set_review(&params.id, review_status, enabled_override)
                .map_err(|e| format!("hub review global: {e}"))
        })?;
        if updated {
            target_db = "global";
            cap = server.with_global_store_read(|store| {
                store
                    .hub_get(&params.id)
                    .map_err(|e| format!("hub get global: {e}"))
            })?;
        }
    }

    if let Some(current_cap) = cap.take() {
        if current_cap.id.starts_with("skill:") {
            if capability_callable(&current_cap) && should_expose_skill_tool(&current_cap) {
                let _ = server.register_skill_tool(&current_cap);
            } else {
                let _ = server.unregister_skill_tool(&current_cap.id);
            }
        }

        cap = Some(current_cap);
    }

    serde_json::to_string(&json!({
        "updated": updated,
        "db": target_db,
        "id": params.id,
        "review_status": review_status,
        "enabled": cap.as_ref().map(|c| c.enabled),
        "health_status": cap.as_ref().map(|c| c.health_status.clone()),
        "callable": cap.as_ref().map(capability_callable),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_hub_set_enabled(
    server: &MemoryServer,
    params: HubSetEnabledParams,
) -> Result<String, String> {
    if params.enabled && crate::builtins::is_retired_builtin_capability_id(&params.id) {
        return Err(format!(
            "Capability '{}' is retired and cannot be enabled.",
            params.id
        ));
    }
    let mut updated = false;
    let mut target_db = "not_found";

    if server.has_project_db() {
        if let Some(existing) = server.with_project_store_read(|store| {
            store
                .hub_get(&params.id)
                .map_err(|e| format!("hub get project: {e}"))
        })? {
            if params.enabled && crate::builtins::is_retired_hub_capability(&existing) {
                return Err(format!(
                    "Capability '{}' (type '{}') is retired and cannot be enabled.",
                    params.id, existing.cap_type
                ));
            }
            let result = server.with_project_store(|store| {
                store
                    .hub_set_enabled(&params.id, params.enabled)
                    .map_err(|e| format!("Failed: {}", e))
            })?;
            if result {
                updated = true;
                target_db = "project";
            }
        }
    }

    if !updated {
        if let Some(existing) = server.with_global_store_read(|store| {
            store
                .hub_get(&params.id)
                .map_err(|e| format!("hub get global: {e}"))
        })? {
            if params.enabled && crate::builtins::is_retired_hub_capability(&existing) {
                return Err(format!(
                    "Capability '{}' (type '{}') is retired and cannot be enabled.",
                    params.id, existing.cap_type
                ));
            }
        }
        let result = server.with_global_store(|store| {
            store
                .hub_set_enabled(&params.id, params.enabled)
                .map_err(|e| format!("Failed: {}", e))
        })?;
        if result {
            updated = true;
            target_db = "global";
        }
    }

    if updated {
        if params.id.starts_with("skill:") {
            if params.enabled {
                if let Ok(cap) = server.get_capability(&params.id) {
                    if should_expose_skill_tool(&cap) {
                        let _ = server.register_skill_tool(&cap);
                    } else {
                        let _ = server.unregister_skill_tool(&cap.id);
                    }
                }
            } else {
                let _ = server.unregister_skill_tool(&params.id);
            }
        }
    }

    serde_json::to_string(&json!({
        "updated": updated,
        "db": target_db,
        "id": params.id,
        "enabled": params.enabled,
    }))
    .map_err(|e| format!("Failed to serialize: {}", e))
}
