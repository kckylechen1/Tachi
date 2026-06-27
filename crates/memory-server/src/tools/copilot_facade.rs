use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::copilot_ops::{handle_tachi_progress_check, handle_tachi_task_brief};
use crate::tool_params::{ProgressCheckParams, TaskBriefParams};
use crate::MemoryServer;

#[tool_router(router = copilot_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Prepare a task brief before non-trivial work: relevant wiki lessons, memory hits, intent, selected_sops, tool_plan, lightweight skill suggestions, and debugging checklist."
    )]
    pub(crate) async fn tachi_task_brief(
        &self,
        Parameters(params): Parameters<TaskBriefParams>,
    ) -> Result<String, String> {
        handle_tachi_task_brief(self, params).await
    }

    #[tool(
        description = "Check whether an agent is stuck after repeated attempts. Returns reframe advice, relevant wiki hits, and an ask-codex prompt when useful. Pass flow_id to append a progress_check event to .tachi/runs/<flow_id>/progress.jsonl."
    )]
    pub(crate) async fn tachi_progress_check(
        &self,
        Parameters(params): Parameters<ProgressCheckParams>,
    ) -> Result<String, String> {
        handle_tachi_progress_check(self, params).await
    }

    #[tool(
        description = "Check whether an agent is stuck after repeated attempts. Returns reframe advice, relevant wiki hits, and an ask-codex prompt when useful. Pass flow_id to append progress.jsonl. (Alias: tachi_progress_check)"
    )]
    pub(crate) async fn tachi_unstick(
        &self,
        Parameters(params): Parameters<ProgressCheckParams>,
    ) -> Result<String, String> {
        handle_tachi_progress_check(self, params).await
    }
}
