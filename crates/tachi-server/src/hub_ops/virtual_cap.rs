use super::discover::hub_discover_inner;
use crate::tool_params::{
    HubDiscoverParams, VirtualCapabilityBindParams, VirtualCapabilityRegisterParams,
    VirtualCapabilityResolveParams,
};
use crate::{DbScope, MemoryServer};
use memcore::{HubCapability, VirtualCapabilityBinding};
use serde_json::json;

pub(crate) async fn handle_vc_register(
    server: &MemoryServer,
    params: VirtualCapabilityRegisterParams,
) -> Result<String, String> {
    if !params.id.starts_with("vc:") {
        return Err("Virtual capability id must start with 'vc:'".to_string());
    }

    let scope = if params.scope == "global" {
        DbScope::Global
    } else if server.has_project_db() {
        DbScope::Project
    } else {
        DbScope::Global
    };

    // ── Cross-scope shadowing guard (I-2) ────────────────────────────────────
    // Prevent registering a VC in one scope when the same ID already exists in
    // the other scope. This avoids a subtle split-brain where bindings in the
    // shadowed scope become unreachable.
    let other_scope = match scope {
        DbScope::Project => Some(DbScope::Global),
        DbScope::Global if server.has_project_db() => Some(DbScope::Project),
        _ => None,
    };
    if let Some(other) = other_scope {
        let exists_in_other = server
            .with_store_for_scope(other, |store| {
                store
                    .hub_get(&params.id)
                    .map(|cap| cap.is_some())
                    .map_err(|e| format!("check other scope: {e}"))
            })
            .unwrap_or(false);
        if exists_in_other {
            return Err(format!(
                "Virtual capability '{}' already exists in {} scope. \
                 Cross-scope VC shadowing is not allowed — it would orphan existing bindings. \
                 Use the existing VC or remove it first with hub_review(enabled=false).",
                params.id,
                other.as_str()
            ));
        }
    }

    let definition = json!({
        "contract": params.contract,
        "routing_strategy": if params.routing_strategy.trim().is_empty() {
            "priority"
        } else {
            params.routing_strategy.as_str()
        },
        "tags": params.tags,
        "input_schema": params.input_schema,
    });

    let cap = HubCapability {
        id: params.id.clone(),
        cap_type: "virtual".to_string(),
        name: params.name,
        // VCs are auto-approved because they are logical routing abstractions, not executable
        // code. Security governance applies at the concrete backend level via sandbox policies
        // and the hub_review gate. The VC layer only resolves *which* backend to call.
        version: 1,
        description: params.description,
        definition: serde_json::to_string(&definition).map_err(|e| format!("serialize: {e}"))?,
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "gateway".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: String::new(),
        updated_at: String::new(),
    };

    server.with_store_for_scope(scope, |store| {
        store
            .hub_register(&cap)
            .map_err(|e| format!("vc register: {e}"))
    })?;

    serde_json::to_string(&json!({
        "registered": true,
        "db": scope.as_str(),
        "id": params.id,
        "cap_type": "virtual",
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_vc_bind(
    server: &MemoryServer,
    params: VirtualCapabilityBindParams,
) -> Result<String, String> {
    let vc_cap = server
        .get_capability(&params.vc_id)
        .map_err(|e| format!("{e}"))?;
    if !vc_cap.cap_type.eq_ignore_ascii_case("virtual") {
        return Err(format!(
            "Capability '{}' is not type 'virtual'",
            params.vc_id
        ));
    }

    let target_cap = server
        .get_capability(&params.capability_id)
        .map_err(|e| format!("{e}"))?;
    if !target_cap.cap_type.eq_ignore_ascii_case("mcp") {
        return Err(format!(
            "Virtual Capability targets must be MCP capabilities, got '{}' for '{}'",
            target_cap.cap_type, params.capability_id
        ));
    }

    let target_db = if server.has_project_db()
        && server.with_project_store_read(|store| {
            store
                .hub_get(&params.vc_id)
                .map(|cap| cap.is_some())
                .map_err(|e| format!("hub get project vc: {e}"))
        })? {
        DbScope::Project
    } else {
        DbScope::Global
    };

    let binding = VirtualCapabilityBinding {
        vc_id: params.vc_id.clone(),
        capability_id: params.capability_id.clone(),
        priority: params.priority,
        version_pin: params.version_pin,
        enabled: params.enabled,
        metadata: params.metadata.unwrap_or_else(|| json!({})),
        created_at: String::new(),
        updated_at: String::new(),
    };

    server.with_store_for_scope(target_db, |store| {
        store
            .vc_upsert_binding(&binding)
            .map_err(|e| format!("vc bind: {e}"))
    })?;

    let resolution = server.resolve_virtual_capability_target(&params.vc_id).ok();

    serde_json::to_string(&json!({
        "updated": true,
        "db": target_db.as_str(),
        "binding": binding,
        "resolution": resolution.map(|(_, report)| report),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_vc_list(
    server: &MemoryServer,
    mut params: HubDiscoverParams,
) -> Result<String, String> {
    params.cap_type = Some("virtual".to_string());
    let mut items = hub_discover_inner(server, &params)?;

    for item in &mut items {
        let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let (bindings, binding_db) = server.get_virtual_capability_bindings(id)?;
        if let Some(obj) = item.as_object_mut() {
            obj.insert(
                "bindings".to_string(),
                serde_json::to_value(bindings).unwrap_or_else(|_| json!([])),
            );
            obj.insert("binding_db".to_string(), json!(binding_db));
        }
    }

    serde_json::to_string(&items).map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_vc_resolve(
    server: &MemoryServer,
    params: VirtualCapabilityResolveParams,
) -> Result<String, String> {
    let (resolved_id, report) = server.resolve_virtual_capability_target(&params.id)?;
    serde_json::to_string(&json!({
        "id": params.id,
        "resolved_id": resolved_id,
        "report": report,
    }))
    .map_err(|e| format!("serialize: {e}"))
}
