use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::dlq_ops::{handle_dlq_list, handle_dlq_retry};
use crate::handoff_ops::handle_handoff_promote_issue;
use crate::skill_chain_ops::handle_chain_skills;
use crate::tool_params::{
    ChainSkillsParams, DlqListParams, DlqRetryParams, HandoffPromoteIssueParams,
    TachiHandoffParams, TachiOrchestratorParams, TachiWorkflowParams,
};
use crate::MemoryServer;

#[tool_router(router = handoff_tool_router, vis = "pub(crate)")]
impl MemoryServer {
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
        description = "Create/link a GitHub issue from an existing handoff memo (action='promote_issue' only). #1099: 'leave'/'check' were retired — use tachi_memory(action='sticky_leave'/'sticky_check') for a short agent-to-agent note, or tachi_orchestrator(action='handoff_write'/'handoff_read') for a structured task baton."
    )]
    pub(crate) async fn tachi_handoff(
        &self,
        Parameters(params): Parameters<TachiHandoffParams>,
    ) -> Result<String, String> {
        let action = params.action.to_ascii_lowercase();
        match action.as_str() {
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
            "leave" | "check" => Err(format!(
                "action='{action}' was retired in #1099. Use tachi_memory(action='sticky_leave'|'sticky_check') \
                 for a short agent-to-agent note, or tachi_orchestrator(action='handoff_write'|'handoff_read') \
                 for a structured task baton. 'promote_issue' is the only action tachi_handoff still supports."
            )),
            _ => Err(format!(
                "Invalid action '{}'. tachi_handoff only supports 'promote_issue'.",
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
