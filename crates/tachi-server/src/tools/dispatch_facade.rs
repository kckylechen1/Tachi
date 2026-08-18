use super::*;

#[tool_router(router = dispatch_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Agent fleet registry (#155): action=list shows claude/codex/grok/kimi; action=select returns heuristic agent + fallback chain for an intent label."
    )]
    pub(crate) async fn tachi_agents(
        &self,
        Parameters(params): Parameters<TachiAgentsParams>,
    ) -> Result<String, String> {
        crate::agent_registry::handle_agents(self, params).await
    }

    #[tool(
        description = "Agent eval scorecard: aggregate live eval success/verification rates by agent, profile, and task type. Fixture replay is local-only and requires TACHI_AGENT_EVAL_ALLOW_FIXTURE=1."
    )]
    pub(crate) async fn tachi_agent_eval(
        &self,
        Parameters(params): Parameters<TachiAgentEvalParams>,
    ) -> Result<String, String> {
        crate::agent_eval::handle_agent_eval(self, params).await
    }
}

#[allow(dead_code)]
impl MemoryServer {
    pub(crate) async fn tachi_complete(
        &self,
        Parameters(params): Parameters<TachiCompleteParams>,
    ) -> Result<String, String> {
        let project_explicit = params.project.is_some();
        crate::complete_ops::handle_tachi_complete(self, params, project_explicit).await
    }
}
