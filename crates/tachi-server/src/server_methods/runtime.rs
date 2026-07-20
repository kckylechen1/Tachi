use crate::server_state::MemoryServer;
use std::sync::{Arc, RwLock as StdRwLock};
use tachi_hub::ToolProfile;

impl MemoryServer {
    pub(crate) fn clone_for_mcp_session(&self) -> Self {
        let mut clone = self.clone();
        let mut runtime = self.agent_runtime_read().clone();
        // #1255: each MCP session gets its own opaque rate-limit identity so
        // burst/RPM windows keyed by session_id do not bleed across clones
        // that still share the process-global RateLimiter.
        runtime.rate_limit_session_id = uuid::Uuid::new_v4().to_string();
        clone.agent_runtime = Arc::new(StdRwLock::new(runtime));
        clone
    }

    /// #1255: opaque rate-limit session id stamped on this server/clone.
    pub(crate) fn rate_limit_session_id(&self) -> String {
        self.agent_runtime_read().rate_limit_session_id.clone()
    }

    pub(crate) fn set_tool_profile(&self, profile: Option<ToolProfile>) {
        self.agent_runtime_write().tool_profile = profile;
    }

    pub(crate) fn active_tool_profile(&self) -> Option<ToolProfile> {
        self.agent_runtime_read().tool_profile
    }

    pub(crate) fn set_session_identity(
        &self,
        client: Option<String>,
        project: Option<String>,
        profile: Option<ToolProfile>,
    ) {
        let mut runtime = self.agent_runtime_write();
        runtime.session_client = client;
        runtime.session_project = project;
        if let Some(profile) = profile {
            runtime.tool_profile = Some(profile);
        }
    }

    pub(crate) fn session_project(&self) -> Option<String> {
        self.agent_runtime_read().session_project.clone()
    }

    /// #1251: stamp this session's raw dispatch recursion-depth marker. Kept a
    /// separate setter from `set_session_identity` (rather than a 4th param)
    /// so the many existing `set_session_identity` call sites are untouched;
    /// only the two callers that actually know the depth — the daemon's
    /// `apply_http_session_identity` (from the wire header) and the CLI
    /// in-process server build (from the process's own env) — set it.
    pub(crate) fn set_session_dispatch_depth(&self, depth: Option<String>) {
        self.agent_runtime_write().session_dispatch_depth = depth;
    }

    /// #1251: the raw dispatch recursion-depth marker for this session, fed to
    /// `session_identity::resolve_dispatch_depth` at the gate and when stamping
    /// a child's depth in `dispatch_ops::mcp_config`.
    pub(crate) fn session_dispatch_depth(&self) -> Option<String> {
        self.agent_runtime_read().session_dispatch_depth.clone()
    }

    pub(crate) fn session_client(&self) -> Option<String> {
        self.agent_runtime_read().session_client.clone()
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
                let visible = tachi_hub::tool_visible(&name, profile, env_patterns.as_deref());
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
