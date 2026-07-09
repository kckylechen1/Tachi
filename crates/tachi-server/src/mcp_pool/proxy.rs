use super::{CircuitProbeDecision, CircuitState};
use crate::server_state::MemoryServer;
use crate::utils::{lock_or_recover, stable_hash};
use chrono::Utc;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tachi_hub::capability_callable;

impl MemoryServer {
    pub(crate) async fn proxy_call_internal(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        self.proxy_call_capability_internal(
            &format!("mcp:{server_name}"),
            None,
            tool_name,
            arguments,
        )
        .await
    }

    pub(crate) async fn proxy_call_capability_internal(
        &self,
        resolved_capability_id: &str,
        requested_capability_id: Option<&str>,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        // 0. Look up capability for deny-list and timeout config
        let server_id = resolved_capability_id.to_string();
        let server_name = resolved_capability_id
            .strip_prefix("mcp:")
            .unwrap_or(resolved_capability_id);
        let args_hash = stable_hash(&format!("{:?}", arguments));
        let audit_reject = |error_kind: &str| {
            let timestamp = Utc::now().to_rfc3339();
            if let Err(err) = self.with_global_store(|store| {
                store
                    .audit_log_insert(
                        &timestamp,
                        server_name,
                        tool_name,
                        &args_hash,
                        false,
                        0,
                        Some(error_kind),
                    )
                    .map_err(|e| format!("{e}"))
            }) {
                tracing::warn!(
                    error = %err,
                    server_name,
                    tool_name,
                    error_kind,
                    "failed to write MCP reject audit log"
                );
            }
        };
        let cap = self.get_capability(&server_id)?;
        if !capability_callable(&cap) {
            audit_reject("capability_not_callable");
            self.record_sandbox_exec_audit(
                &server_id,
                "preflight",
                "denied",
                Some("capability is not callable"),
                0,
                Some(tool_name),
                Some("capability_not_callable"),
                &json!({
                    "server_name": server_name,
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
        let cap_def: serde_json::Value = serde_json::from_str(&cap.definition)
            .map_err(|e| rmcp::ErrorData::internal_error(format!("bad definition: {e}"), None))?;
        if crate::mcp_connection::is_bigmodel_remote_mcp(&cap_def)
            || crate::mcp_connection::is_remote_http_mcp(&cap_def)
        {
            let result = self
                .proxy_call_bigmodel_mcp(&server_id, &cap_def, tool_name, arguments.clone())
                .await?;
            if !result.is_error.unwrap_or(false) {
                crate::pipeline_ops::schedule_auto_ingest_from_mcp(
                    self,
                    &server_id,
                    tool_name,
                    &cap_def,
                    arguments.as_ref(),
                    &result,
                );
            }
            return Ok(result);
        }
        let (sandbox_policy, policy_source) =
            self.get_effective_sandbox_policy(requested_capability_id, &server_id);
        if sandbox_policy.is_none() {
            audit_reject("policy_missing");
            self.record_sandbox_exec_audit(
                &server_id,
                "preflight",
                "denied",
                Some("missing sandbox policy"),
                0,
                Some(tool_name),
                Some("policy_missing"),
                &json!({
                    "server_name": server_name,
                    "requested_capability_id": requested_capability_id,
                }),
            );
            return Err(rmcp::ErrorData::invalid_params(
                format!(
                    "Capability '{}' has no sandbox policy. Use sandbox_set_policy before calling.",
                    server_id
                ),
                None,
            ));
        }
        if let Some(policy) = sandbox_policy.as_ref() {
            let policy_enabled = policy
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if !policy_enabled {
                audit_reject("policy_disabled");
                self.record_sandbox_exec_audit(
                    &server_id,
                    "preflight",
                    "denied",
                    Some("sandbox policy disabled capability"),
                    0,
                    Some(tool_name),
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
        }

        // 1. Check allow/deny permissions
        if let Some(allow_list) = cap_def["permissions"]["allow"].as_array() {
            let allowed: HashSet<&str> = allow_list.iter().filter_map(|v| v.as_str()).collect();
            if !allowed.is_empty() && !allowed.contains(tool_name) {
                audit_reject("permission_allow_denied");
                self.record_sandbox_exec_audit(
                    &server_id,
                    "preflight",
                    "denied",
                    Some("tool not in permissions.allow"),
                    0,
                    Some(tool_name),
                    Some("permission_allow_denied"),
                    &json!({
                        "server_name": server_name,
                        "requested_capability_id": requested_capability_id,
                    }),
                );
                return Err(rmcp::ErrorData::invalid_params(
                    format!(
                        "Tool '{}' is not in permissions.allow for '{}'",
                        tool_name, server_name
                    ),
                    None,
                ));
            }
        }

        if let Some(deny_list) = cap_def["permissions"]["deny"].as_array() {
            let denied: Vec<&str> = deny_list.iter().filter_map(|v| v.as_str()).collect();
            if denied.contains(&tool_name) {
                audit_reject("permission_deny_blocked");
                self.record_sandbox_exec_audit(
                    &server_id,
                    "preflight",
                    "denied",
                    Some("tool denied by permissions policy"),
                    0,
                    Some(tool_name),
                    Some("permission_deny_blocked"),
                    &json!({
                        "server_name": server_name,
                        "requested_capability_id": requested_capability_id,
                    }),
                );
                return Err(rmcp::ErrorData::invalid_params(
                    format!(
                        "Tool '{}' is denied by permissions policy on '{}'",
                        tool_name, server_name
                    ),
                    None,
                ));
            }
        }

        // 2. Check circuit breaker. Once an open circuit cools down, allow
        // exactly one half-open probe until it succeeds, fails, or exits early.
        let _half_open_probe = match self.pool.acquire_circuit_probe(server_name, Instant::now()) {
            CircuitProbeDecision::Allowed(probe) => probe,
            CircuitProbeDecision::Open => {
                audit_reject("circuit_open");
                self.record_sandbox_exec_audit(
                    &server_id,
                    "preflight",
                    "denied",
                    Some("circuit breaker open"),
                    0,
                    Some(tool_name),
                    Some("circuit_open"),
                    &json!({
                        "server_name": server_name,
                        "requested_capability_id": requested_capability_id,
                    }),
                );
                return Err(rmcp::ErrorData::internal_error(
                    format!("Circuit open for '{}', retry after cooldown", server_name),
                    None,
                ));
            }
            CircuitProbeDecision::ProbeInProgress => {
                audit_reject("circuit_half_open_probe_in_progress");
                self.record_sandbox_exec_audit(
                    &server_id,
                    "preflight",
                    "denied",
                    Some("circuit breaker half-open probe already in progress"),
                    0,
                    Some(tool_name),
                    Some("circuit_half_open_probe_in_progress"),
                    &json!({
                        "server_name": server_name,
                        "requested_capability_id": requested_capability_id,
                    }),
                );
                return Err(rmcp::ErrorData::internal_error(
                    format!(
                        "Circuit half-open probe already in progress for '{}'",
                        server_name
                    ),
                    None,
                ));
            }
        };

        // 3. Acquire per-child concurrency permit (rebuild if max_concurrency changed)
        let semaphore = {
            let mut state = lock_or_recover(&self.pool.state, "mcp_pool.state");
            let mut max_conc = cap_def["max_concurrency"].as_u64().unwrap_or(1);
            if let Some(policy_cap) = sandbox_policy
                .as_ref()
                .and_then(|v| v.get("max_concurrency"))
                .and_then(|v| v.as_u64())
            {
                max_conc = std::cmp::min(max_conc.max(1), policy_cap.max(1));
            }
            let max_conc = max_conc.max(1) as usize;
            let needs_rebuild = state
                .semaphores
                .get(server_name)
                .map(|(_, cached_max)| *cached_max != max_conc)
                .unwrap_or(true);
            if needs_rebuild {
                state.semaphores.insert(
                    server_name.to_string(),
                    (Arc::new(tokio::sync::Semaphore::new(max_conc)), max_conc),
                );
            }
            state
                .semaphores
                .get(server_name)
                .map(|(semaphore, _)| Arc::clone(semaphore))
                .ok_or_else(|| {
                    rmcp::ErrorData::internal_error(
                        format!("semaphore missing after initialization for {server_name}"),
                        None,
                    )
                })?
        };
        let _permit = semaphore
            .acquire()
            .await
            .map_err(|_| rmcp::ErrorData::internal_error("semaphore closed", None))?;

        // 4. Ensure connection exists (atomic check-and-connect to avoid TOCTOU race)
        self.ensure_child_connected_with_context(&server_id, requested_capability_id)
            .await?;

        // 5. Get peer and call tool with timeout
        let mut call_params = rmcp::model::CallToolRequestParams::new(tool_name.to_string());
        if let Some(ref args) = arguments {
            call_params = call_params.with_arguments(args.clone());
        }

        let peer = {
            let mut state = lock_or_recover(&self.pool.state, "mcp_pool.state");
            if let Some(conn) = state.connections.get_mut(server_name) {
                conn.last_used = Instant::now();
                conn.client.peer().clone()
            } else {
                return Err(rmcp::ErrorData::internal_error("connection lost", None));
            }
        };

        let mut timeout_ms = cap_def["tool_timeout_ms"].as_u64().unwrap_or(30000);
        if let Some(policy_tool_ms) = sandbox_policy
            .as_ref()
            .and_then(|v| v.get("max_tool_ms"))
            .and_then(|v| v.as_u64())
        {
            timeout_ms = std::cmp::min(timeout_ms.max(1), policy_tool_ms.max(1));
        }
        let start = Instant::now();

        let result = tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            peer.call_tool(call_params),
        )
        .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        // 6. Process result, update circuit breaker, log audit
        let (final_result, sandbox_decision, sandbox_error_kind) = match result {
            Ok(Ok(r)) => {
                if !r.is_error.unwrap_or(false) {
                    crate::pipeline_ops::schedule_auto_ingest_from_mcp(
                        self,
                        &server_id,
                        tool_name,
                        &cap_def,
                        arguments.as_ref(),
                        &r,
                    );
                }
                // Tool returned successfully (even if r.is_error — that's a tool-level error, not transport)
                let mut state = lock_or_recover(&self.pool.state, "mcp_pool.state");
                state
                    .circuits
                    .insert(server_name.to_string(), (CircuitState::Closed, 0));
                (Ok(r), "allowed", None)
            }
            Ok(Err(e)) => {
                // Transport/protocol error — increment circuit breaker
                self.record_circuit_failure(server_name);
                (
                    Err(rmcp::ErrorData::internal_error(
                        format!("proxy call failed: {e}"),
                        None,
                    )),
                    "failed",
                    Some("proxy_failed"),
                )
            }
            Err(_timeout) => {
                // Timeout — increment circuit breaker
                self.record_circuit_failure(server_name);
                (
                    Err(rmcp::ErrorData::internal_error(
                        format!(
                            "Tool call '{}' on '{}' timed out after {}ms",
                            tool_name, server_name, timeout_ms
                        ),
                        None,
                    )),
                    "timeout",
                    Some("tool_timeout"),
                )
            }
        };

        // 7. Audit log (fire and forget)
        let success = final_result.is_ok();
        let error_kind = final_result.as_ref().err().map(|e| format!("{e}"));
        let timestamp = Utc::now().to_rfc3339();
        if let Err(err) = self.with_global_store(|store| {
            store
                .audit_log_insert(
                    &timestamp,
                    server_name,
                    tool_name,
                    &args_hash,
                    success,
                    duration_ms,
                    error_kind.as_deref(),
                )
                .map_err(|e| format!("{e}"))
        }) {
            tracing::warn!(
                error = %err,
                server_name,
                tool_name,
                "failed to write MCP tool audit log"
            );
        }
        self.record_sandbox_exec_audit(
            &server_id,
            "tool_call",
            sandbox_decision,
            error_kind.as_deref(),
            duration_ms,
            Some(tool_name),
            sandbox_error_kind,
            &json!({
                "server_name": server_name,
                "requested_capability_id": requested_capability_id,
                "policy_source": if policy_source.is_empty() { None::<String> } else { Some(policy_source) },
                "timeout_ms": timeout_ms,
            }),
        );

        final_result
    }

    pub(super) fn record_circuit_failure(&self, server_name: &str) {
        let mut state = lock_or_recover(&self.pool.state, "mcp_pool.state");
        let entry = state
            .circuits
            .entry(server_name.to_string())
            .or_insert((CircuitState::Closed, 0));
        entry.1 += 1;
        if entry.1 >= 3 || matches!(entry.0, CircuitState::HalfOpen { .. }) {
            entry.0 = CircuitState::Open {
                until: Instant::now() + Duration::from_secs(30),
            };
            state.connections.remove(server_name);
        }
    }
}
