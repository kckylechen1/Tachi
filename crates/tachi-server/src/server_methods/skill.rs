use crate::hub_ops::{build_skill_execution_envelope, execute_registered_skill_prompt};
use crate::mcp_proxy::McpToolExposureMode;
use crate::server_state::MemoryServer;
use crate::shared_defs::dlq_mutation_is_unsafe;
use crate::utils::lock_or_recover;
use memcore::HubCapability;
use serde_json::Value;
use tachi_hub::{build_skill_tool_from_cap, make_text_tool_result};

impl MemoryServer {
    pub(crate) fn register_skill_tool(&self, cap: &HubCapability) -> Result<String, String> {
        if let Err(error) = self.unregister_skill_tool(&cap.id) {
            tracing::warn!(cap_id = %cap.id, error = %error, "failed to clear existing skill tool before register");
        }
        let (tool_name, tool) = build_skill_tool_from_cap(cap)?;
        {
            lock_or_recover(&self.tool_discovery.skill_tools, "skill_tools")
                .insert(tool_name.clone(), cap.id.clone());
            lock_or_recover(&self.tool_discovery.skill_tool_defs, "skill_tool_defs")
                .insert(tool_name.clone(), tool);
        }
        Ok(tool_name)
    }

    pub(crate) fn unregister_skill_tool(&self, skill_id: &str) -> Result<Option<String>, String> {
        let removed_tool_name = {
            let mut skill_tools = lock_or_recover(&self.tool_discovery.skill_tools, "skill_tools");
            let tool_name = skill_tools
                .iter()
                .find(|(_, id)| id.as_str() == skill_id)
                .map(|(name, _)| name.clone());
            if let Some(ref name) = tool_name {
                skill_tools.remove(name);
                lock_or_recover(&self.tool_discovery.skill_tool_defs, "skill_tool_defs")
                    .remove(name);
            }
            tool_name
        };

        Ok(removed_tool_name)
    }

    pub(crate) async fn call_skill_tool(
        &self,
        tool_name: &str,
        arguments: Option<rmcp::model::JsonObject>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        let skill_id = lock_or_recover(&self.tool_discovery.skill_tools, "skill_tools")
            .get(tool_name)
            .cloned()
            .ok_or_else(|| {
                rmcp::ErrorData::invalid_params(
                    format!("Skill tool '{}' not found", tool_name),
                    None,
                )
            })?;

        let args = Value::Object(arguments.unwrap_or_default().into_iter().collect());
        let execution = execute_registered_skill_prompt(self, &skill_id, &args)
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;

        make_text_tool_result(&build_skill_execution_envelope(
            &skill_id,
            execution,
            Some(tool_name),
        ))
    }

    pub(crate) async fn retry_dispatch(
        &self,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        if lock_or_recover(&self.tool_discovery.skill_tools, "skill_tools").contains_key(tool_name)
        {
            let args_obj = arguments.map(|m| m.into_iter().collect::<rmcp::model::JsonObject>());
            return self.call_skill_tool(tool_name, args_obj).await;
        }

        if dlq_mutation_is_unsafe(tool_name, arguments.as_ref())
            || self.tool_router.has_route(tool_name)
        {
            return Err(rmcp::ErrorData::invalid_params(
                format!(
                    "Tool '{}' cannot be retried via DLQ because it is native or non-idempotent; retry the MCP call explicitly",
                    tool_name
                ),
                None,
            ));
        }

        let args_obj = arguments.map(|m| m.into_iter().collect::<rmcp::model::JsonObject>());

        if let Some((server_name, remote_tool)) = {
            let proxy_tools = lock_or_recover(&self.tool_discovery.proxy_tools, "proxy_tools");
            crate::server_handler::split_proxy_tool_name(
                tool_name,
                proxy_tools.keys().map(String::as_str),
            )
        } {
            let exposure_mode = self.proxy_tool_exposure_mode_for_server(&server_name)?;
            if exposure_mode == McpToolExposureMode::Gateway {
                return Err(rmcp::ErrorData::invalid_params(
                    format!(
                        "Direct proxy tool '{}' is disabled by tool_exposure=gateway for '{}'. Retry via hub_call.",
                        tool_name, server_name
                    ),
                    None,
                ));
            }
            return self
                .proxy_call_internal(&server_name, &remote_tool, args_obj)
                .await;
        }

        Err(rmcp::ErrorData::invalid_params(
            format!(
                "Native tool '{}' cannot be retried via DLQ — retry the MCP call directly",
                tool_name
            ),
            None,
        ))
    }
}
