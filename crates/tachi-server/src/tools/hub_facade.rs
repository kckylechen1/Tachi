use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::capability_ops::{
    handle_prepare_capability_bundle, handle_recommend_capability, handle_recommend_skill,
    handle_recommend_toolchain,
};
use crate::hub_ops::{
    handle_distill_trajectory, handle_export_skills, handle_hub_call, handle_hub_disconnect,
    handle_hub_discover, handle_hub_feedback, handle_hub_get, handle_hub_quick_add,
    handle_hub_register, handle_hub_review, handle_hub_set_active_version, handle_hub_set_enabled,
    handle_hub_stats, handle_run_skill, handle_skill_evolve, handle_tachi_audit_log,
    handle_vc_bind, handle_vc_list, handle_vc_register, handle_vc_resolve,
};
use crate::tool_params::*;
use crate::MemoryServer;

#[tool_router(router = hub_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(description = "Enable or disable a Hub capability by ID.")]
    pub(crate) async fn hub_set_enabled(
        &self,
        Parameters(params): Parameters<HubSetEnabledParams>,
    ) -> Result<String, String> {
        handle_hub_set_enabled(self, params).await
    }

    #[tool(description = "Set governance review status for a Hub capability.")]
    pub(crate) async fn hub_review(
        &self,
        Parameters(params): Parameters<HubReviewParams>,
    ) -> Result<String, String> {
        handle_hub_review(self, params).await
    }

    #[tool(description = "Route an alias capability ID to a concrete active capability version.")]
    pub(crate) async fn hub_set_active_version(
        &self,
        Parameters(params): Parameters<HubSetActiveVersionParams>,
    ) -> Result<String, String> {
        handle_hub_set_active_version(self, params).await
    }

    #[tool(
        description = "Export Hub skills to agent-specific file formats. Targets: claude (SKILL.md + symlinks), openclaw (plugin manifest), cursor (.mdc rules), generic (raw files)."
    )]
    pub(crate) async fn hub_export_skills(
        &self,
        Parameters(params): Parameters<ExportSkillsParams>,
    ) -> Result<String, String> {
        handle_export_skills(self, params).await
    }

    #[tool(
        description = "Register a Virtual Capability (logical capability layer) on top of concrete backends."
    )]
    pub(crate) async fn vc_register(
        &self,
        Parameters(params): Parameters<VirtualCapabilityRegisterParams>,
    ) -> Result<String, String> {
        handle_vc_register(self, params).await
    }

    #[tool(
        description = "Bind a Virtual Capability to a concrete capability with deterministic priority and optional version pin."
    )]
    pub(crate) async fn vc_bind(
        &self,
        Parameters(params): Parameters<VirtualCapabilityBindParams>,
    ) -> Result<String, String> {
        handle_vc_bind(self, params).await
    }

    #[tool(description = "List Virtual Capabilities together with their current bindings.")]
    pub(crate) async fn vc_list(
        &self,
        Parameters(params): Parameters<HubDiscoverParams>,
    ) -> Result<String, String> {
        handle_vc_list(self, params).await
    }

    #[tool(
        description = "Resolve a Virtual Capability to the concrete capability currently selected for routing."
    )]
    pub(crate) async fn vc_resolve(
        &self,
        Parameters(params): Parameters<VirtualCapabilityResolveParams>,
    ) -> Result<String, String> {
        handle_vc_resolve(self, params).await
    }

    #[tool(description = "Register a capability (skill, plugin, or MCP server) in the Hub.")]
    pub(crate) async fn hub_register(
        &self,
        Parameters(params): Parameters<HubRegisterParams>,
    ) -> Result<String, String> {
        handle_hub_register(self, params).await
    }

    #[tool(
        description = "Composite: hub_register followed by an optional hub_review approve+enable. \
                       Honors the trusted-command allowlist — auto_approve is silently dropped \
                       (with a warning) for untrusted stdio MCP commands."
    )]
    pub(crate) async fn hub_quick_add(
        &self,
        Parameters(params): Parameters<HubQuickAddParams>,
    ) -> Result<String, String> {
        handle_hub_quick_add(self, params).await
    }

    #[tool(
        description = "Discover available Hub capabilities across skills, plugins, and MCP servers. For skill workflow discovery, prefer canonical tachi_skill(action='discover'); this direct hub_discover route remains callable for general Hub/backcompat use."
    )]
    pub(crate) async fn hub_discover(
        &self,
        Parameters(params): Parameters<HubDiscoverParams>,
    ) -> Result<String, String> {
        handle_hub_discover(self, params).await
    }

    #[tool(description = "Get a specific capability from the Hub by ID.")]
    pub(crate) async fn hub_get(
        &self,
        Parameters(params): Parameters<HubGetParams>,
    ) -> Result<String, String> {
        handle_hub_get(self, params).await
    }

    #[tool(description = "Record feedback for a Hub capability invocation.")]
    pub(crate) async fn hub_feedback(
        &self,
        Parameters(params): Parameters<HubFeedbackParams>,
    ) -> Result<String, String> {
        handle_hub_feedback(self, params).await
    }

    #[tool(description = "Get Hub capability statistics and metrics.")]
    pub(crate) async fn hub_stats(&self) -> Result<String, String> {
        handle_hub_stats(self).await
    }

    #[tool(
        description = "Distill a completed task trajectory into a reusable Skill, persist a permanent skill snapshot, and register/update the distilled Hub Skill."
    )]
    pub(crate) async fn distill_trajectory(
        &self,
        Parameters(params): Parameters<DistillTrajectoryParams>,
    ) -> Result<String, String> {
        handle_distill_trajectory(self, params).await
    }

    #[tool(
        description = "Evolve a skill by analyzing its telemetry and using LLM to produce an improved prompt. Creates a new versioned capability."
    )]
    pub(crate) async fn skill_evolve(
        &self,
        Parameters(params): Parameters<SkillEvolveParams>,
    ) -> Result<String, String> {
        handle_skill_evolve(self, params).await
    }

    #[tool(
        description = "Recommend the best Tachi capability for a task query. Uses Hub metadata, visibility, host constraints, and telemetry to rank candidate capabilities."
    )]
    pub(crate) async fn recommend_capability(
        &self,
        Parameters(params): Parameters<RecommendCapabilityParams>,
    ) -> Result<String, String> {
        handle_recommend_capability(self, params).await
    }

    #[tool(
        description = "Recommend the most relevant skills for a task query. Returns ranked skill candidates plus callable tool aliases when available."
    )]
    pub(crate) async fn recommend_skill(
        &self,
        Parameters(params): Parameters<RecommendSkillParams>,
    ) -> Result<String, String> {
        handle_recommend_skill(self, params).await
    }

    #[tool(
        description = "Recommend a host-aware toolchain for a task query, including skills, supporting capabilities, projected packs, and suggested host-native execution tools."
    )]
    pub(crate) async fn recommend_toolchain(
        &self,
        Parameters(params): Parameters<RecommendToolchainParams>,
    ) -> Result<String, String> {
        handle_recommend_toolchain(self, params).await
    }

    #[tool(description = "View audit log of proxy tool calls through the Hub.")]
    pub(crate) async fn tachi_audit_log(
        &self,
        Parameters(params): Parameters<AuditLogParams>,
    ) -> Result<String, String> {
        handle_tachi_audit_log(self, params).await
    }

    #[tool(
        description = "Call a tool on a registered MCP server through the Hub using the shared connection pool."
    )]
    pub(crate) async fn hub_call(
        &self,
        Parameters(params): Parameters<HubCallParams>,
    ) -> Result<String, String> {
        handle_hub_call(self, params).await
    }

    #[tool(
        description = "Disconnect a cached MCP server connection from the pool. Forces a fresh reconnect (with updated env/config) on next hub_call."
    )]
    pub(crate) async fn hub_disconnect(
        &self,
        Parameters(params): Parameters<HubDisconnectParams>,
    ) -> Result<String, String> {
        handle_hub_disconnect(self, params).await
    }
}

#[allow(dead_code)]
impl MemoryServer {
    pub(crate) async fn run_skill(
        &self,
        Parameters(params): Parameters<RunSkillParams>,
    ) -> Result<String, String> {
        handle_run_skill(self, params).await
    }

    pub(crate) async fn prepare_capability_bundle(
        &self,
        Parameters(params): Parameters<PrepareCapabilityBundleParams>,
    ) -> Result<String, String> {
        handle_prepare_capability_bundle(self, params).await
    }
}
