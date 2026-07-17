use super::super::*;

impl MemoryServer {
    pub(crate) async fn discover_mcp_tools(
        &self,
        capability_id: &str,
        def: &serde_json::Value,
    ) -> Result<Vec<rmcp::model::Tool>, String> {
        if is_remote_http_mcp(def) {
            return self
                .proxy_list_remote_http_mcp_tools(capability_id, def)
                .await;
        }

        let discovery_timeout = self.tool_discovery.mcp_discovery_timeout;
        let client = self
            .connect_mcp_service(capability_id, None, def, discovery_timeout)
            .await?;
        let list_result =
            tokio::time::timeout(discovery_timeout, client.peer().list_all_tools()).await;
        let cancel_result = client.cancel().await;

        match list_result {
            Ok(Ok(tools)) => {
                if let Err(error) = cancel_result {
                    tracing::warn!(
                        capability_id = %capability_id,
                        error = %error,
                        "failed to cancel MCP discovery client after successful tool list"
                    );
                }
                Ok(tools)
            }
            Ok(Err(e)) => {
                if let Err(error) = cancel_result {
                    tracing::warn!(
                        capability_id = %capability_id,
                        error = %error,
                        "failed to cancel MCP discovery client after list_tools error"
                    );
                }
                Err(format!("list_tools failed: {e}"))
            }
            Err(_) => {
                if let Err(error) = cancel_result {
                    tracing::warn!(
                        capability_id = %capability_id,
                        error = %error,
                        "failed to cancel MCP discovery client after timeout"
                    );
                }
                Err(format!(
                    "list_tools timed out after {}ms",
                    discovery_timeout.as_millis()
                ))
            }
        }
    }
}
