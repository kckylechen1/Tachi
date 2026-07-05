use crate::profiles::ToolProfile;
use crate::server_state::MemoryServer;
use std::sync::Arc;

impl MemoryServer {
    pub(crate) fn set_tool_profile(&self, profile: Option<ToolProfile>) {
        self.agent_runtime_write().tool_profile = profile;
    }

    pub(crate) fn active_tool_profile(&self) -> Option<ToolProfile> {
        self.agent_runtime_read().tool_profile
    }

    pub(crate) fn native_tool_visibility(&self) -> Vec<(String, String, bool)> {
        let env_patterns = crate::server_handler::current_exposed_tool_patterns();
        let profile = self.active_tool_profile();
        let mut tools = self
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| {
                let name = tool.name.as_ref().to_string();
                let description = tool
                    .description
                    .as_ref()
                    .map(|text| crate::utils::compact_text_line(text.as_ref(), 96))
                    .unwrap_or_default();
                let visible =
                    crate::profiles::tool_visible(&name, profile, env_patterns.as_deref());
                (name, description, visible)
            })
            .collect::<Vec<_>>();
        tools.sort_by(|a, b| a.0.cmp(&b.0));
        tools
    }

    /// Stamp "now" as the last MCP activity. Lock-free; called centrally from
    /// `ServerHandler::call_tool` on every tool invocation (including calls a
    /// stdio child forwards to this daemon).
    pub(crate) fn touch_activity(&self) {
        self.last_activity_ms.store(
            chrono::Utc::now().timestamp_millis(),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// Shared handle to the last-activity clock, for the serve-loop idle reaper.
    pub(crate) fn activity_clock(&self) -> Arc<std::sync::atomic::AtomicI64> {
        Arc::clone(&self.last_activity_ms)
    }
}
