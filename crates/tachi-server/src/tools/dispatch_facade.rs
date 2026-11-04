use super::*;

#[tool_router(router = dispatch_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Read available Staff execution backends and dispatch profiles with action=list or profiles. The host selects the worker; this registry does not choose an agent from task keywords."
    )]
    pub(crate) async fn tachi_agents(
        &self,
        Parameters(params): Parameters<TachiAgentsParams>,
    ) -> Result<String, String> {
        crate::agent_registry::handle_agents(self, params).await
    }

    #[tool(
        description = "Native-worker experience ledger: register a bounded run, observe its actual result, adjudicate with evidence, get its record, or read candidate_projection for model-specific advisory experience. The host owns model choice and worker execution. Standard exposes only these five actions; operator reporting/replay and host attachment retain separate access policy."
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
        crate::task_lifecycle::ownership::producer_preflight(self, params.flow_id.as_deref())?;
        crate::complete_ops::handle_tachi_complete(self, params, project_explicit).await
    }
}
