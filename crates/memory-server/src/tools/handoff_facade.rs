use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::dlq_ops::{handle_dlq_list, handle_dlq_retry};
use crate::handoff_ops::{
    handle_handoff_check, handle_handoff_leave, handle_handoff_promote_issue,
};
use crate::skill_chain_ops::handle_chain_skills;
use crate::tool_params::{
    ChainSkillsParams, DlqListParams, DlqRetryParams, HandoffCheckParams, HandoffLeaveParams,
    HandoffPromoteIssueParams, TachiHandoffParams, TachiOrchestratorParams, TachiWorkflowParams,
};
use crate::MemoryServer;

#[tool_router(router = handoff_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Leave a handoff memo for the next agent session. Contains session summary, next steps, and optional context."
    )]
    pub(crate) async fn handoff_leave(
        &self,
        Parameters(params): Parameters<HandoffLeaveParams>,
    ) -> Result<String, String> {
        handle_handoff_leave(self, params).await
    }

    #[tool(
        description = "Check for pending handoff memos from previous agent sessions. Call this at the start of a new session."
    )]
    pub(crate) async fn handoff_check(
        &self,
        Parameters(params): Parameters<HandoffCheckParams>,
    ) -> Result<String, String> {
        handle_handoff_check(self, params).await
    }

    #[tool(
        description = "Execute a chain of skills in sequence (Unix pipe style). Output of each skill feeds as input to the next."
    )]
    pub(crate) async fn chain_skills(
        &self,
        Parameters(params): Parameters<ChainSkillsParams>,
    ) -> Result<String, String> {
        handle_chain_skills(self, params).await
    }

    #[tool(
        description = "List dead letter queue entries (failed tool calls). Filter by status: pending, retrying, resolved, abandoned."
    )]
    pub(crate) async fn dlq_list(
        &self,
        Parameters(params): Parameters<DlqListParams>,
    ) -> Result<String, String> {
        handle_dlq_list(self, params).await
    }

    #[tool(
        description = "Manually retry a dead letter queue entry by its ID. Re-dispatches the failed tool call."
    )]
    pub(crate) async fn dlq_retry(
        &self,
        Parameters(params): Parameters<DlqRetryParams>,
    ) -> Result<String, String> {
        handle_dlq_retry(self, params).await
    }

    #[tool(
        description = "Unified handoff: 'leave' a memo, 'check' pending memos, or 'promote_issue' to create/link a GitHub issue from a handoff memo."
    )]
    pub(crate) async fn tachi_handoff(
        &self,
        Parameters(params): Parameters<TachiHandoffParams>,
    ) -> Result<String, String> {
        let action = params.action.to_ascii_lowercase();
        match action.as_str() {
            "leave" => {
                let summary = params
                    .summary
                    .clone()
                    .ok_or_else(|| "summary is required when action='leave'".to_string())?;
                let leave_params = HandoffLeaveParams {
                    summary,
                    next_steps: params.next_steps.clone(),
                    target_agent: params.target_agent.clone(),
                    context: params.context.clone(),
                };
                handle_handoff_leave(self, leave_params).await
            }
            "check" => {
                let check_params = HandoffCheckParams {
                    agent_id: params.agent_id.clone(),
                    acknowledge: params.acknowledge,
                };
                handle_handoff_check(self, check_params).await
            }
            "promote_issue" => {
                let memo_id = params
                    .memo_id
                    .clone()
                    .ok_or_else(|| "memo_id is required when action='promote_issue'".to_string())?;
                let repo = params
                    .repo
                    .clone()
                    .ok_or_else(|| "repo is required when action='promote_issue'".to_string())?;
                let promote_params = HandoffPromoteIssueParams {
                    memo_id,
                    repo,
                    title: params.title.clone(),
                    labels: params.labels.clone(),
                    flow_id: params.flow_id.clone(),
                    force: params.force,
                };
                handle_handoff_promote_issue(self, promote_params).await
            }
            _ => Err(format!(
                "Invalid action '{}'. Use 'leave', 'check', or 'promote_issue'.",
                params.action
            )),
        }
    }

    #[tool(
        description = "Issue→Doc→Memory closure: action=close_loop writes wiki with references[] (issue + docs + related issues); build_references previews the array. Replaces nightly wiki compile (#77)."
    )]
    pub(crate) async fn tachi_workflow(
        &self,
        Parameters(params): Parameters<TachiWorkflowParams>,
    ) -> Result<String, String> {
        crate::workflow_closure::handle_workflow(self, params).await
    }

    #[tool(
        description = "Persistent orchestrator state outside LLM context: todo_list, todo_update, handoff_write, handoff_read, recovery_briefing. Stored in hard_state (survives compaction). Use task_id = dispatch_id or issue id."
    )]
    pub(crate) async fn tachi_orchestrator(
        &self,
        Parameters(params): Parameters<TachiOrchestratorParams>,
    ) -> Result<String, String> {
        crate::orchestrator_ops::handle_orchestrator(self, params).await
    }
}
