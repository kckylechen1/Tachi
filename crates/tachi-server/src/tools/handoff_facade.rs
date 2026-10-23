use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::dlq_ops::{handle_dlq_list, handle_dlq_retry};
use crate::skill_chain_ops::handle_chain_skills;
use crate::tool_params::{ChainSkillsParams, DlqListParams, DlqRetryParams};
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
}
