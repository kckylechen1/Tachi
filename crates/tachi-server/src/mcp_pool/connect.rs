use super::ChildConnection;
use crate::server_state::MemoryServer;
use crate::utils::lock_or_recover;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tachi_hub::capability_callable;

impl MemoryServer {
    pub(crate) async fn ensure_child_connected_with_context(
        &self,
        resolved_capability_id: &str,
        requested_capability_id: Option<&str>,
    ) -> Result<(), rmcp::ErrorData> {
        let server_name = resolved_capability_id
            .strip_prefix("mcp:")
            .unwrap_or(resolved_capability_id);
        // Check under lock
        {
            let state = lock_or_recover(&self.pool.state, "mcp_pool.state");
            if state.connections.contains_key(server_name) {
                return Ok(());
            }
        }
        // Not connected — acquire connecting lock to serialize connection attempts
        let connecting_lock = {
            let mut state = lock_or_recover(&self.pool.state, "mcp_pool.state");
            state
                .connecting_locks
                .entry(server_name.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = connecting_lock.lock().await;
        // Double-check after acquiring lock
        {
            let state = lock_or_recover(&self.pool.state, "mcp_pool.state");
            if state.connections.contains_key(server_name) {
                return Ok(());
            }
        }
        self.connect_child_with_context(resolved_capability_id, requested_capability_id)
            .await
    }

    pub(crate) async fn connect_child_with_context(
        &self,
        resolved_capability_id: &str,
        requested_capability_id: Option<&str>,
    ) -> Result<(), rmcp::ErrorData> {
        let server_id = resolved_capability_id.to_string();
        let server_name = resolved_capability_id
            .strip_prefix("mcp:")
            .unwrap_or(resolved_capability_id);

        let cap = self.get_capability(&server_id)?;
        let (sandbox_policy, policy_source) =
            self.get_effective_sandbox_policy(requested_capability_id, &server_id);
        if sandbox_policy.is_none() {
            self.record_sandbox_exec_audit(
                &server_id,
                "preflight",
                "denied",
                Some("missing sandbox policy"),
                0,
                None,
                Some("policy_missing"),
                &json!({
                    "server_name": server_name,
                    "requested_capability_id": requested_capability_id,
                }),
            );
            return Err(rmcp::ErrorData::invalid_params(
                format!(
                    "Capability '{}' has no sandbox policy. Use sandbox_set_policy before connecting.",
                    server_id
                ),
                None,
            ));
        }
        if sandbox_policy
            .as_ref()
            .and_then(|v| v.get("enabled"))
            .and_then(|v| v.as_bool())
            == Some(false)
        {
            self.record_sandbox_exec_audit(
                &server_id,
                "preflight",
                "denied",
                Some("sandbox policy disabled capability"),
                0,
                None,
                Some("policy_disabled"),
                &json!({
                    "server_name": server_name,
                    "requested_capability_id": requested_capability_id,
                    "policy_source": if policy_source.is_empty() { None::<String> } else { Some(policy_source.clone()) },
                }),
            );
            return Err(rmcp::ErrorData::invalid_params(
                format!(
                    "Capability '{}' blocked by sandbox policy (enabled=false)",
                    server_id
                ),
                None,
            ));
        }
        if !capability_callable(&cap) {
            self.record_sandbox_exec_audit(
                &server_id,
                "preflight",
                "denied",
                Some("capability is not callable"),
                0,
                None,
                Some("capability_not_callable"),
                &json!({
                    "server_name": server_name,
                    "requested_capability_id": requested_capability_id,
                    "enabled": cap.enabled,
                    "review_status": cap.review_status,
                    "health_status": cap.health_status,
                }),
            );
            return Err(rmcp::ErrorData::invalid_params(
                format!(
                    "MCP server '{}' is not callable (enabled={}, review_status={}, health_status={}).",
                    server_id, cap.enabled, cap.review_status, cap.health_status
                ),
                None,
            ));
        }

        let def: serde_json::Value = serde_json::from_str(&cap.definition)
            .map_err(|e| rmcp::ErrorData::internal_error(format!("bad definition: {e}"), None))?;
        let startup_timeout_ms = def["startup_timeout_ms"].as_u64().unwrap_or(30_000);
        let startup_timeout = Duration::from_millis(startup_timeout_ms.max(1));
        let client = self
            .connect_mcp_service(&server_id, requested_capability_id, &def, startup_timeout)
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;

        lock_or_recover(&self.pool.state, "mcp_pool.state")
            .connections
            .insert(
                server_name.to_string(),
                ChildConnection {
                    client,
                    last_used: Instant::now(),
                },
            );
        Ok(())
    }
}
