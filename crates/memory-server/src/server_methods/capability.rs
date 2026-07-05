use super::ResolvedCallTarget;
use crate::mcp_proxy::{resolve_mcp_tool_exposure, McpToolExposureMode};
use crate::server_state::MemoryServer;
use chrono::Utc;
use memory_core::{HubCapability, VirtualCapabilityBinding};
use serde_json::{json, Value};
use tachi_hub::capability_callable;

impl MemoryServer {
    pub(crate) fn get_capability(&self, cap_id: &str) -> Result<HubCapability, rmcp::ErrorData> {
        let mut found = None;
        if self.has_project_db() {
            found = self
                .with_project_store_read(|store| {
                    store
                        .hub_get(cap_id)
                        .map_err(|e| format!("hub get project: {e}"))
                })
                .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        }
        if found.is_none() {
            found = self
                .with_global_store_read(|store| {
                    store
                        .hub_get(cap_id)
                        .map_err(|e| format!("hub get global: {e}"))
                })
                .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        }
        found.ok_or_else(|| {
            rmcp::ErrorData::invalid_params(format!("Capability '{cap_id}' not found"), None)
        })
    }

    pub(crate) fn resolve_active_capability_id(
        &self,
        cap_id: &str,
    ) -> Result<String, rmcp::ErrorData> {
        if self.has_project_db() {
            let route = self
                .with_project_store_read(|store| {
                    store
                        .hub_get_active_version_route(cap_id)
                        .map_err(|e| format!("hub route project: {e}"))
                })
                .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
            if let Some(target) = route {
                return Ok(target);
            }
        }

        let route = self
            .with_global_store_read(|store| {
                store
                    .hub_get_active_version_route(cap_id)
                    .map_err(|e| format!("hub route global: {e}"))
            })
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;

        Ok(route.unwrap_or_else(|| cap_id.to_string()))
    }

    pub(crate) fn get_virtual_capability_bindings(
        &self,
        vc_id: &str,
    ) -> Result<(Vec<VirtualCapabilityBinding>, &'static str), String> {
        if self.has_project_db() {
            let bindings = self.with_project_store_read(|store| {
                store
                    .vc_list_bindings(vc_id)
                    .map_err(|e| format!("vc bindings project: {e}"))
            })?;
            if !bindings.is_empty() {
                return Ok((bindings, "project"));
            }
        }

        let bindings = self.with_global_store_read(|store| {
            store
                .vc_list_bindings(vc_id)
                .map_err(|e| format!("vc bindings global: {e}"))
        })?;
        Ok((bindings, "global"))
    }

    pub(crate) fn resolve_virtual_capability_target(
        &self,
        vc_id: &str,
    ) -> Result<(String, Value), String> {
        let vc_cap = self.get_capability(vc_id).map_err(|e| format!("{e}"))?;

        if !vc_cap.cap_type.eq_ignore_ascii_case("virtual") {
            return Err(format!("Capability '{vc_id}' is not type 'virtual'"));
        }
        if !capability_callable(&vc_cap) {
            return Err(format!(
                "Virtual Capability '{}' is not callable (enabled={}, review_status={}, health_status={}).",
                vc_id, vc_cap.enabled, vc_cap.review_status, vc_cap.health_status
            ));
        }

        let (bindings, binding_db) = self.get_virtual_capability_bindings(vc_id)?;
        if bindings.is_empty() {
            return Err(format!("Virtual Capability '{vc_id}' has no bindings"));
        }

        let mut chosen: Option<String> = None;
        let mut candidates = Vec::new();

        for binding in bindings {
            let mut reason = None;
            let mut version = None;
            let mut cap_type = None;

            if !binding.enabled {
                reason = Some("binding_disabled".to_string());
            }

            let target_cap = match self.get_capability(&binding.capability_id) {
                Ok(cap) => {
                    version = Some(cap.version);
                    cap_type = Some(cap.cap_type.clone());
                    Some(cap)
                }
                Err(_) => {
                    reason = Some("target_missing".to_string());
                    None
                }
            };

            if reason.is_none() {
                if let Some(cap) = target_cap.as_ref() {
                    if !cap.cap_type.eq_ignore_ascii_case("mcp") {
                        reason = Some("target_not_mcp".to_string());
                    } else if let Some(pin) = binding.version_pin {
                        if cap.version != pin {
                            reason = Some("version_pin_mismatch".to_string());
                        }
                    }

                    if reason.is_none() && !capability_callable(cap) {
                        reason = Some("target_not_callable".to_string());
                    }
                }
            }

            if chosen.is_none() && reason.is_none() {
                chosen = Some(binding.capability_id.clone());
            }

            candidates.push(json!({
                "vc_id": binding.vc_id,
                "capability_id": binding.capability_id,
                "priority": binding.priority,
                "version_pin": binding.version_pin,
                "enabled": binding.enabled,
                "target_version": version,
                "target_type": cap_type,
                "selected": reason.is_none() && chosen.as_deref() == Some(binding.capability_id.as_str()),
                "status": reason.unwrap_or_else(|| "ok".to_string()),
                "metadata": binding.metadata,
            }));
        }

        let selected = chosen.ok_or_else(|| {
            format!(
                "Virtual Capability '{vc_id}' has no callable MCP binding. Inspect vc_resolve for candidate status."
            )
        })?;

        Ok((
            selected.clone(),
            json!({
                "id": vc_id,
                "binding_db": binding_db,
                "selected": selected,
                "candidates": candidates,
            }),
        ))
    }

    pub(crate) fn resolve_call_target(
        &self,
        requested_id: &str,
    ) -> Result<ResolvedCallTarget, String> {
        if let Ok(cap) = self.get_capability(requested_id) {
            if cap.cap_type.eq_ignore_ascii_case("virtual") {
                let (resolved_id, resolution) =
                    self.resolve_virtual_capability_target(requested_id)?;
                return Ok(ResolvedCallTarget {
                    requested_id: requested_id.to_string(),
                    resolved_id,
                    requested_kind: "virtual".to_string(),
                    resolution,
                });
            }
        }

        let resolved_id = self
            .resolve_active_capability_id(requested_id)
            .map_err(|e| format!("{e}"))?;
        let requested_kind = if requested_id == resolved_id {
            "concrete"
        } else {
            "alias"
        };
        Ok(ResolvedCallTarget {
            requested_id: requested_id.to_string(),
            requested_kind: requested_kind.to_string(),
            resolved_id: resolved_id.clone(),
            resolution: json!({
                "id": requested_id,
                "selected": resolved_id,
                "kind": requested_kind,
            }),
        })
    }

    pub(crate) fn record_capability_call_outcome(
        &self,
        cap_id: &str,
        success: bool,
        error_kind: Option<&str>,
    ) -> Result<(), String> {
        const OPEN_THRESHOLD: u32 = 3;

        if self.has_project_db() {
            let in_project = self.with_project_store_read(|store| {
                store
                    .hub_get(cap_id)
                    .map(|cap| cap.is_some())
                    .map_err(|e| format!("hub get project: {e}"))
            })?;
            if in_project {
                return self.with_project_store(|store| {
                    store
                        .hub_record_call_outcome(cap_id, success, error_kind, OPEN_THRESHOLD)
                        .map_err(|e| format!("hub call outcome project: {e}"))
                });
            }
        }

        self.with_global_store(|store| {
            store
                .hub_record_call_outcome(cap_id, success, error_kind, OPEN_THRESHOLD)
                .map_err(|e| format!("hub call outcome global: {e}"))
        })
    }

    pub(crate) fn proxy_tool_exposure_mode_for_server(
        &self,
        server_name: &str,
    ) -> Result<McpToolExposureMode, rmcp::ErrorData> {
        let cap_id = format!("mcp:{server_name}");
        let cap = self.get_capability(&cap_id)?;
        let def: serde_json::Value = serde_json::from_str(&cap.definition).map_err(|e| {
            rmcp::ErrorData::internal_error(
                format!("bad definition for capability '{cap_id}': {e}"),
                None,
            )
        })?;
        Ok(resolve_mcp_tool_exposure(
            &def,
            self.tool_discovery.mcp_tool_exposure_mode,
        ))
    }

    pub(crate) fn get_sandbox_policy_for_capability(&self, capability_id: &str) -> Option<Value> {
        if self.has_project_db() {
            match self.with_project_store(|store| {
                store
                    .get_sandbox_policy(capability_id)
                    .map_err(|e| format!("sandbox policy project: {e}"))
            }) {
                Ok(Some(policy)) => return Some(policy),
                Ok(None) => {}
                Err(e) => {
                    eprintln!(
                        "[sandbox] failed to read project policy for '{}': {}",
                        capability_id, e
                    );
                }
            }
        }

        match self.with_global_store(|store| {
            store
                .get_sandbox_policy(capability_id)
                .map_err(|e| format!("sandbox policy global: {e}"))
        }) {
            Ok(policy) => policy,
            Err(e) => {
                eprintln!(
                    "[sandbox] failed to read global policy for '{}': {}",
                    capability_id, e
                );
                None
            }
        }
    }

    pub(crate) fn get_effective_sandbox_policy(
        &self,
        requested_capability_id: Option<&str>,
        resolved_capability_id: &str,
    ) -> (Option<Value>, String) {
        if let Some(policy) = self.get_sandbox_policy_for_capability(resolved_capability_id) {
            return (Some(policy), resolved_capability_id.to_string());
        }

        if let Some(requested_id) = requested_capability_id {
            if requested_id != resolved_capability_id {
                if let Some(policy) = self.get_sandbox_policy_for_capability(requested_id) {
                    return (Some(policy), requested_id.to_string());
                }
            }
        }

        (None, String::new())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_sandbox_exec_audit(
        &self,
        capability_id: &str,
        stage: &str,
        decision: &str,
        reason: Option<&str>,
        duration_ms: u64,
        tool_name: Option<&str>,
        error_kind: Option<&str>,
        metadata: &Value,
    ) {
        let timestamp = Utc::now().to_rfc3339();
        let metadata_json = serde_json::to_string(metadata).unwrap_or_else(|_| "{}".to_string());
        if let Err(e) = self.with_global_store(|store| {
            store
                .insert_sandbox_exec_audit(
                    &timestamp,
                    capability_id,
                    stage,
                    decision,
                    reason,
                    duration_ms,
                    tool_name,
                    error_kind,
                    &metadata_json,
                )
                .map_err(|err| format!("sandbox exec audit insert: {err}"))
        }) {
            eprintln!(
                "[sandbox] failed to record execution audit for '{}' (stage={}, decision={}): {}",
                capability_id, stage, decision, e
            );
        }
    }
}
