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
                let _ = cancel_result;
                Ok(tools)
            }
            Ok(Err(e)) => Err(format!("list_tools failed: {e}")),
            Err(_) => Err(format!(
                "list_tools timed out after {}ms",
                discovery_timeout.as_millis()
            )),
        }
    }
}
