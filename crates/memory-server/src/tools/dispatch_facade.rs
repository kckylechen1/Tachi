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
        description = "Dispatch a task to a delegate CLI agent or typed host adapter (claude, codex, grok, kimi, opencode, or custom). Assembles prompt with context from memory/wiki + injected skills, spawns agent subprocess, returns structured result. Call tachi_complete afterwards to record the eval."
    )]
    pub(crate) async fn tachi_dispatch(
        &self,
        Parameters(params): Parameters<TachiDispatchParams>,
    ) -> Result<String, String> {
        crate::dispatch_ops::handle_tachi_dispatch(self, params).await
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

    #[tool(
        description = "View the task board (kanban) showing all dispatched background tasks and their statuses. Returns a list of tasks with their A2A state (WORKING, COMPLETED, FAILED, etc)."
    )]
    pub(crate) async fn tachi_board(
        &self,
        Parameters(params): Parameters<TachiBoardParams>,
    ) -> Result<String, String> {
        crate::dispatch_ops::handle_tachi_board(self, params).await
    }

    #[tool(
        description = "Merge a git worktree branch back to the main branch and optionally remove the worktree. Use after reviewing tachi_dispatch results."
    )]
    pub(crate) async fn approve_merge(
        &self,
        Parameters(params): Parameters<TachiApproveMergeParams>,
    ) -> Result<String, String> {
        crate::dispatch_ops::handle_approve_merge(params).await
    }

    #[tool(
        description = "Declare task completion and write an entry to the eval ledger. Records agent, outcome, duration, cost, skills used, and (optionally) trajectory/diff for later distillation. Returns a review bundle. Does NOT auto-merge worktrees — use approve_merge for that."
    )]
    pub(crate) async fn tachi_complete(
        &self,
        Parameters(params): Parameters<TachiCompleteParams>,
    ) -> Result<String, String> {
        crate::complete_ops::handle_tachi_complete(self, params).await
    }
}
