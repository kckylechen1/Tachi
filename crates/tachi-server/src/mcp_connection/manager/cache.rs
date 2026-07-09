use super::super::*;

impl MemoryServer {
    pub(crate) fn clear_proxy_tools(&self, server_name: &str) {
        lock_or_recover(&self.tool_discovery.proxy_tools, "proxy_tools").remove(server_name);
    }

    pub(crate) fn cache_proxy_tools(&self, server_name: &str, tools: Vec<rmcp::model::Tool>) {
        lock_or_recover(&self.tool_discovery.proxy_tools, "proxy_tools")
            .insert(server_name.to_string(), tools);
    }
}
