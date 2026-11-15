use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::hub_ops::{
    handle_export_skills, handle_hub_discover, handle_hub_feedback, handle_hub_get,
    handle_hub_quick_add, handle_hub_register, handle_hub_review, handle_hub_set_enabled,
    handle_hub_stats, handle_tachi_audit_log,
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

    #[tool(
        description = "Export Hub skills to agent-specific file formats. Targets: claude (SKILL.md + symlinks), openclaw (plugin manifest), cursor (.mdc rules), generic (raw files)."
    )]
    pub(crate) async fn hub_export_skills(
        &self,
        Parameters(params): Parameters<ExportSkillsParams>,
    ) -> Result<String, String> {
        handle_export_skills(self, params).await
    }

    #[tool(description = "Register a capability (skill or plugin) in the Hub.")]
    pub(crate) async fn hub_register(
        &self,
        Parameters(params): Parameters<HubRegisterParams>,
    ) -> Result<String, String> {
        handle_hub_register(self, params).await
    }

    #[tool(
        description = "Composite: hub_register followed by an optional hub_review approve+enable."
    )]
    pub(crate) async fn hub_quick_add(
        &self,
        Parameters(params): Parameters<HubQuickAddParams>,
    ) -> Result<String, String> {
        handle_hub_quick_add(self, params).await
    }

    #[tool(
        description = "Discover available Hub capabilities across skills and plugins. For skill workflow discovery, prefer canonical tachi_skill(action='discover'); this direct hub_discover route remains callable for general Hub/backcompat use."
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

    #[tool(description = "View audit log of proxy tool calls through the Hub.")]
    pub(crate) async fn tachi_audit_log(
        &self,
        Parameters(params): Parameters<AuditLogParams>,
    ) -> Result<String, String> {
        handle_tachi_audit_log(self, params).await
    }
}
