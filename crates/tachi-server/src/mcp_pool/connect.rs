use super::ChildConnection;
use crate::server_state::MemoryServer;
use crate::utils::{lock_or_recover, stable_hash};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tachi_hub::{capability_callable, capability_not_callable_reason};

impl MemoryServer {
    pub(crate) fn mcp_connection_key(
        &self,
        cap: &memcore::HubCapability,
        requested_capability_id: Option<&str>,
        policy: Option<&serde_json::Value>,
    ) -> String {
        let identity = json!({
            "global": self.global_db_path_buf(),
            "project": self.project_db_path_buf(),
            "definition": cap.definition,
            "requested": requested_capability_id,
            "policy": policy,
        });
        format!(
            "{}#{}",
            cap.id.strip_prefix("mcp:").unwrap_or(&cap.id),
            stable_hash(&identity.to_string())
        )
    }

    pub(crate) async fn ensure_child_connected_with_context(
        &self,
        cap: &memcore::HubCapability,
        requested_capability_id: Option<&str>,
        sandbox_policy: Option<&serde_json::Value>,
        policy_source: &str,
    ) -> Result<rmcp::service::Peer<rmcp::service::RoleClient>, rmcp::ErrorData> {
        self.ensure_mcp_snapshot_current(cap, requested_capability_id, sandbox_policy)?;
        let key = self.mcp_connection_key(cap, requested_capability_id, sandbox_policy);
        let connecting_lock = {
            let mut state = lock_or_recover(&self.pool.state, "mcp_pool.state");
            if let Some(connection) = state.connections.get_mut(&key) {
                connection.last_used = Instant::now();
                return Ok(connection.client.peer().clone());
            }
            state
                .connecting_locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = connecting_lock.lock().await;
        self.ensure_mcp_snapshot_current(cap, requested_capability_id, sandbox_policy)?;
        {
            let mut state = lock_or_recover(&self.pool.state, "mcp_pool.state");
            if let Some(connection) = state.connections.get_mut(&key) {
                connection.last_used = Instant::now();
                return Ok(connection.client.peer().clone());
            }
        }
        self.connect_child_with_context(cap, requested_capability_id, sandbox_policy, policy_source)
            .await
    }

    fn ensure_mcp_snapshot_current(
        &self,
        cap: &memcore::HubCapability,
        requested_capability_id: Option<&str>,
        policy: Option<&serde_json::Value>,
    ) -> Result<(), rmcp::ErrorData> {
        let current = self.get_capability(&cap.id)?;
        let (current_policy, _) =
            self.get_effective_sandbox_policy(requested_capability_id, &cap.id);
        if current.definition != cap.definition
            || !capability_callable(&current)
            || current_policy.as_ref() != policy
        {
            return Err(rmcp::ErrorData::invalid_params(
                "MCP connection changed while waiting; retry with the current configuration",
                Some(json!({"code":"mcp_connection_changed"})),
            ));
        }
        Ok(())
    }

    pub(crate) async fn connect_child_with_context(
        &self,
        cap: &memcore::HubCapability,
        requested_capability_id: Option<&str>,
        sandbox_policy: Option<&serde_json::Value>,
        policy_source: &str,
    ) -> Result<rmcp::service::Peer<rmcp::service::RoleClient>, rmcp::ErrorData> {
        let server_id = cap.id.clone();
        let server_name = cap.id.strip_prefix("mcp:").unwrap_or(&cap.id);
        let def: serde_json::Value = serde_json::from_str(&cap.definition)
            .map_err(|e| rmcp::ErrorData::internal_error(format!("bad definition: {e}"), None))?;
        if sandbox_policy.is_none() && !crate::mcp_connection::is_remote_http_mcp(&def) {
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
                    "Capability '{}' has no sandbox policy. Configure its local process policy before connecting.",
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
                    "policy_source": if policy_source.is_empty() { None::<String> } else { Some(policy_source.to_owned()) },
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
        if !capability_callable(cap) {
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
            let failing_field =
                capability_not_callable_reason(cap).unwrap_or_else(|| "unknown gate".to_string());
            return Err(rmcp::ErrorData::invalid_params(
                format!(
                    "MCP server '{}' is not callable (enabled={}, review_status={}, health_status={}); failing gate: {}.",
                    server_id, cap.enabled, cap.review_status, cap.health_status, failing_field
                ),
                None,
            ));
        }

        let startup_timeout_ms = def["startup_timeout_ms"].as_u64().unwrap_or(30_000);
        let startup_timeout = Duration::from_millis(startup_timeout_ms.max(1));
        let client = self
            .connect_mcp_service(
                &server_id,
                requested_capability_id,
                &def,
                startup_timeout,
                (sandbox_policy.cloned(), policy_source.to_owned()),
            )
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;

        let peer = client.peer().clone();
        let key = self.mcp_connection_key(cap, requested_capability_id, sandbox_policy);
        lock_or_recover(&self.pool.state, "mcp_pool.state")
            .connections
            .insert(
                key,
                ChildConnection {
                    client,
                    last_used: Instant::now(),
                },
            );
        Ok(peer)
    }
}
