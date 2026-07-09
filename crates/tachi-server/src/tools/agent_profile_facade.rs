use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::agent_profile_ops::handle_tachi_profile;
use crate::foundry_ops::{
    handle_list_agent_evolution_proposals, handle_project_agent_profile,
    handle_queue_agent_evolution, handle_review_agent_evolution_proposal,
    handle_synthesize_agent_evolution,
};
use crate::project_db_ops::handle_tachi_init_project_db;
use crate::tool_params::{
    AgentRegisterParams, AgentWhoamiParams, InitProjectDbParams, ListAgentEvolutionProposalsParams,
    ProjectAgentProfileParams, ReviewAgentEvolutionProposalParams, SynthesizeAgentEvolutionParams,
    TachiProfileParams,
};
use crate::{AgentProfile, MemoryServer};

#[tool_router(router = agent_profile_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Synthesize agent evolution proposals from canonical profile documents and evidence. Returns structured JSON proposals; use dry_run=true to inspect the normalized request without calling the model."
    )]
    pub(crate) async fn synthesize_agent_evolution(
        &self,
        Parameters(params): Parameters<SynthesizeAgentEvolutionParams>,
    ) -> Result<String, String> {
        handle_synthesize_agent_evolution(self, params).await
    }

    #[tool(
        description = "Queue an agent evolution synthesis job. Persists job state and stores generated proposals for later review."
    )]
    pub(crate) async fn queue_agent_evolution(
        &self,
        Parameters(params): Parameters<SynthesizeAgentEvolutionParams>,
    ) -> Result<String, String> {
        handle_queue_agent_evolution(self, params).await
    }

    #[tool(
        description = "List persisted agent evolution proposals for a target agent. Optionally filter by review status."
    )]
    pub(crate) async fn list_agent_evolution_proposals(
        &self,
        Parameters(params): Parameters<ListAgentEvolutionProposalsParams>,
    ) -> Result<String, String> {
        handle_list_agent_evolution_proposals(self, params).await
    }

    #[tool(
        description = "Review a persisted agent evolution proposal by marking it approved, rejected, or applied."
    )]
    pub(crate) async fn review_agent_evolution_proposal(
        &self,
        Parameters(params): Parameters<ReviewAgentEvolutionProposalParams>,
    ) -> Result<String, String> {
        handle_review_agent_evolution_proposal(self, params).await
    }

    #[tool(
        description = "Project approved agent evolution proposals into host documents. Returns projected content and can optionally write back to disk paths."
    )]
    pub(crate) async fn project_agent_profile(
        &self,
        Parameters(params): Parameters<ProjectAgentProfileParams>,
    ) -> Result<String, String> {
        handle_project_agent_profile(self, params).await
    }

    #[tool(
        description = "Import and render canonical AgentProfilePack projections for user-agent alignment. Read-only: returns dry-run AGENTS.md / CLAUDE.md / GEMINI.md / Cursor/OpenClaw projections or a bounded runtime context block; it never writes files."
    )]
    pub(crate) async fn tachi_profile(
        &self,
        Parameters(params): Parameters<TachiProfileParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(self, "tachi_profile", &params).await?
        {
            return Ok(body);
        }
        handle_tachi_profile(self, params).await
    }

    #[tool(
        description = "Initialize a project-scoped Tachi memory DB under the current or target git repository."
    )]
    pub(crate) async fn tachi_init_project_db(
        &self,
        Parameters(params): Parameters<InitProjectDbParams>,
    ) -> Result<String, String> {
        handle_tachi_init_project_db(self, params).await
    }

    #[tool(
        description = "Register this agent session with an identity profile. Enables per-agent memory scoping, tool filtering, and rate limit customization."
    )]
    pub(crate) async fn agent_register(
        &self,
        Parameters(params): Parameters<AgentRegisterParams>,
    ) -> Result<String, String> {
        let profile = AgentProfile {
            agent_id: params.agent_id.clone(),
            display_name: params
                .display_name
                .unwrap_or_else(|| params.agent_id.clone()),
            capabilities: params.capabilities,
            tool_filter: params.tool_filter,
            rate_limit_rpm: params.rate_limit_rpm,
            rate_limit_burst: params.rate_limit_burst,
            registered_at: Utc::now().to_rfc3339(),
        };

        let response = serde_json::to_string(&serde_json::json!({
            "status": "registered",
            "agent_id": profile.agent_id,
            "display_name": profile.display_name,
            "capabilities": profile.capabilities,
            "tool_filter": profile.tool_filter,
            "rate_limit_rpm": profile.rate_limit_rpm,
            "rate_limit_burst": profile.rate_limit_burst,
            "registered_at": profile.registered_at,
        }))
        .map_err(|e| format!("serialize: {e}"))?;

        let mut guard = self.agent_runtime_write();
        guard.agent_profile = Some(profile);

        Ok(response)
    }

    #[tool(
        description = "Return the current agent profile for this session, or null if no agent has registered."
    )]
    pub(crate) async fn agent_whoami(
        &self,
        Parameters(_params): Parameters<AgentWhoamiParams>,
    ) -> Result<String, String> {
        let guard = self.agent_runtime_read();
        match guard.agent_profile.as_ref() {
            Some(profile) => serde_json::to_string(&profile).map_err(|e| format!("serialize: {e}")),
            None => Ok(r#"{"status":"unregistered","message":"No agent profile set. Call agent_register to identify this session."}"#.to_string()),
        }
    }
}
